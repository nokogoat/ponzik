//! Rotary speaker (Leslie 122 style): a tube amp, an 800 Hz crossover, a
//! treble horn and a bass drum spinning at their own speeds.
//!
//! Each rotor carries its sound around a circle: the path to each of the two
//! microphones lengthens and shortens (Doppler vibrato), the horn's beam
//! sweeps past them (amplitude and brightness), and a reflection off the
//! cabinet walls arrives later with the opposite motion. The rotors have
//! mass: the light horn reaches its speed in about a second, the heavy drum
//! takes several, and they slow down even more lazily — the speed ramps are
//! the sound. Shared by the tonewheel organ and the `rotary` effect.

use std::f32::consts::TAU;

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::filter::OnePole;
use ponzik_dsp::oversample::Oversampler2x;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::{DEFAULT_SR, Lr4, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

/// Speed of sound, m/s.
const SOUND: f32 = 343.0;
/// Crossover between drum and horn, Hz.
const CROSSOVER: f32 = 800.0;

/// Rotor speeds of a Leslie 122, in turns per second: horn chorale 48 rpm and
/// tremolo 400 rpm, drum chorale 40 rpm and tremolo 340 rpm.
pub(crate) const HORN_SLOW: f32 = 0.8;
pub(crate) const HORN_FAST: f32 = 6.67;
pub(crate) const DRUM_SLOW: f32 = 0.67;
pub(crate) const DRUM_FAST: f32 = 5.67;

/// Time to cover 90 % of a speed change, in seconds (inertia 1): the horn
/// spins up in about a second, the drum in four and a half; both coast down
/// more slowly than they spin up.
pub(crate) const HORN_UP: f32 = 1.0;
pub(crate) const HORN_DOWN: f32 = 1.6;
pub(crate) const DRUM_UP: f32 = 4.5;
pub(crate) const DRUM_DOWN: f32 = 5.5;

/// Radius of the sound source's circle, m: the horn mouth swings wide, the
/// drum's rotating baffle much less.
pub(crate) const HORN_RADIUS: f32 = 0.15;
pub(crate) const DRUM_RADIUS: f32 = 0.07;

/// Depth of the amplitude modulation as the source faces away (0: none).
const HORN_AM: f32 = 0.55;
const DRUM_AM: f32 = 0.3;

/// Makeup gains of the two paths for the average loss of the beam.
const HORN_MAKEUP: f32 = 2.2;
const DRUM_MAKEUP: f32 = 1.25;

/// Longest delay a tap reads, s (base + swing + reflection).
const MAX_DELAY: f32 = 0.012;
/// Base path of the direct sound, s.
const BASE_DELAY: f32 = 0.0015;

/// Rotor speed setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Speed {
    Stop,
    Slow,
    Fast,
}

impl Speed {
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Speed::Stop,
            1 => Speed::Slow,
            _ => Speed::Fast,
        }
    }
}

/// A spinning rotor: angle and speed with inertia.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rotor {
    /// Angle in turns.
    pub angle: f32,
    /// Speed in turns per second.
    pub speed: f32,
    target: f32,
    up: f32,
    down: f32,
    slow: f32,
    fast: f32,
    t_up: f32,
    t_down: f32,
}

impl Rotor {
    fn new(slow: f32, fast: f32, t_up: f32, t_down: f32, angle: f32) -> Self {
        Self { angle, speed: slow, target: slow, up: 0.0, down: 0.0, slow, fast, t_up, t_down }
    }

    fn set_rates(&mut self, inertia: f32, sample_rate: f32) {
        // 90 % in t: coefficient of a one-pole with tau = t / ln 10.
        let coef = |t: f32| 1.0 - (-std::f32::consts::LN_10 / (t * inertia * sample_rate).max(1.0)).exp();
        self.up = coef(self.t_up);
        self.down = coef(self.t_down);
    }

    fn set_speed(&mut self, speed: Speed) {
        self.target = match speed {
            Speed::Stop => 0.0,
            Speed::Slow => self.slow,
            Speed::Fast => self.fast,
        };
    }

    #[inline]
    fn tick(&mut self, sample_rate: f32) {
        let k = if self.target > self.speed { self.up } else { self.down };
        self.speed += (self.target - self.speed) * k;
        if (self.speed - self.target).abs() < 2e-3 {
            self.speed = self.target;
        }
        self.angle += self.speed / sample_rate;
        if self.angle >= 1.0 {
            self.angle -= 1.0;
        }
    }
}

/// One rotor's sound path to the two microphones: a delay line read at the
/// moving distance, direct and reflected, plus the beam's brightness.
#[derive(Clone, Debug)]
struct Path {
    line: DelayLine,
    /// Swing of the path length, samples (radius / c).
    swing: f32,
    am: f32,
    /// The horn's beam is brighter on axis; the drum's baffle is not.
    beam: bool,
    /// Darker copy for the brightness sweep, per mic and tap.
    dull: [OnePole; 4],
}

impl Path {
    fn new(radius: f32, am: f32, beam: bool, sample_rate: f32) -> Self {
        let max = (MAX_DELAY * sample_rate) as usize + 8;
        Self {
            line: DelayLine::new(max),
            swing: radius / SOUND * sample_rate,
            am,
            beam,
            dull: std::array::from_fn(|_| OnePole::new(1_800.0, sample_rate)),
        }
    }

    fn clear(&mut self) {
        self.line.clear();
        self.dull.iter_mut().for_each(OnePole::reset);
    }

    /// Writes `x`, reads what the mics at `mics` (turns) hear for a rotor
    /// at `angle`, with reflections at `reflect` level `extra` samples later.
    #[inline]
    fn hear(&mut self, x: f32, angle: f32, mics: [f32; 2], base: f32, extra: f32, reflect: f32) -> (f32, f32) {
        self.line.write(x);
        let mut out = [0.0; 2];
        for (m, o) in out.iter_mut().enumerate() {
            for tap in 0..2 {
                // The reflection comes from the wall behind: half a turn away.
                let rel = angle - mics[m] + 0.5 * tap as f32;
                let c = fast_cos(rel);
                // Facing the mic: shortest path (c = 1).
                let d = base + extra * tap as f32 + self.swing * (1.0 - c);
                let s = self.line.read_frac(d);
                let facing = 0.5 + 0.5 * c;
                let g = (1.0 - self.am) + self.am * facing;
                let s = if self.beam {
                    let lp = self.dull[m * 2 + tap].lowpass(s);
                    lp + (s - lp) * facing * facing
                } else {
                    s
                };
                *o += s * g * if tap == 0 { 1.0 } else { reflect };
            }
        }
        (out[0], out[1])
    }
}

/// Cosine of an angle in turns (any real), within 0.1 %.
#[inline]
fn fast_cos(turns: f32) -> f32 {
    crate::instruments::common::fast_sin(turns + 0.25)
}

/// Tube preamp and power amp: asymmetric soft clipping, 2× oversampled.
#[derive(Clone, Debug, Default)]
pub(crate) struct TubeDrive {
    os: Oversampler2x,
    dc: f32,
    dc_coef: f32,
}

impl TubeDrive {
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.dc_coef = 1.0 - (-TAU * 10.0 / sample_rate).exp();
        self.reset();
    }

    pub fn reset(&mut self) {
        self.os.reset();
        self.dc = 0.0;
    }

    /// `drive` in [0, 1]: clean to growling; levels stay comparable.
    #[inline]
    pub fn process(&mut self, x: f32, drive: f32) -> f32 {
        if drive <= 0.001 {
            return x;
        }
        let gain = 1.0 + 24.0 * drive * drive;
        let bias = 0.18 * drive;
        let off = (gain * bias).tanh();
        let makeup = 1.0 / gain.powf(0.85);
        let y = self.os.process(x, |v| {
            let u = gain * v + gain * bias;
            // A tube clips its two halves differently.
            let s = if u > 0.0 { u.tanh() } else { 0.85 * (u / 0.85).tanh() };
            (s - off) * makeup
        });
        self.dc += (y - self.dc) * self.dc_coef;
        flush_denormal(y - self.dc)
    }
}

/// The rotating speaker itself: mono in, stereo out.
pub(crate) struct RotarySpeaker {
    sample_rate: f32,
    xover: Lr4,
    /// The horn's own response: no deep lows, rolls off past 6 kHz.
    horn_eq: Biquad,
    pub horn: Rotor,
    pub drum: Rotor,
    horn_path: Path,
    drum_path: Path,
    inertia: f32,
    width: f32,
    distance: f32,
    /// Mic angles, turns.
    mics: [f32; 2],
    /// Reflection level and extra delay (samples).
    reflect: f32,
    extra: f32,
    horn_gain: f32,
    drum_gain: f32,
    /// Without reflections (measurements).
    #[cfg(test)]
    pub dry_paths: bool,
}

impl RotarySpeaker {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            sample_rate,
            xover: Lr4::default(),
            horn_eq: Biquad::default(),
            horn: Rotor::new(HORN_SLOW, HORN_FAST, HORN_UP, HORN_DOWN, 0.0),
            drum: Rotor::new(DRUM_SLOW, DRUM_FAST, DRUM_UP, DRUM_DOWN, 0.37),
            horn_path: Path::new(HORN_RADIUS, HORN_AM, true, sample_rate),
            drum_path: Path::new(DRUM_RADIUS, DRUM_AM, false, sample_rate),
            inertia: 1.0,
            width: 0.7,
            distance: 0.4,
            mics: [0.0, 0.0],
            reflect: 0.3,
            extra: 0.0,
            horn_gain: 1.0,
            drum_gain: 1.0,
            #[cfg(test)]
            dry_paths: false,
        };
        s.set_sample_rate(sample_rate);
        s
    }

    /// Allocates the delay lines.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.xover.set(CROSSOVER, sample_rate);
        self.horn_eq.coefs = BiquadCoefs::lowpass(6_000.0_f32.min(sample_rate * 0.45), 0.6, sample_rate);
        self.horn_path = Path::new(HORN_RADIUS, HORN_AM, true, sample_rate);
        self.drum_path = Path::new(DRUM_RADIUS, DRUM_AM, false, sample_rate);
        self.set_inertia(self.inertia);
        self.set_mics(self.width, self.distance);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.xover.reset();
        self.horn_eq.reset();
        self.horn_path.clear();
        self.drum_path.clear();
    }

    pub fn set_speed(&mut self, speed: Speed) {
        self.horn.set_speed(speed);
        self.drum.set_speed(speed);
    }

    /// Inertia scale: 1 is a Leslie 122, 0.25 four times nimbler.
    pub fn set_inertia(&mut self, inertia: f32) {
        self.inertia = inertia;
        self.horn.set_rates(inertia, self.sample_rate);
        self.drum.set_rates(inertia, self.sample_rate);
    }

    /// `width` in [0, 1]: the two mics from one spot to opposite sides;
    /// `distance` in [0, 1]: close (dry) to far (more cabinet reflection).
    pub fn set_mics(&mut self, width: f32, distance: f32) {
        self.width = width.clamp(0.0, 1.0);
        self.distance = distance.clamp(0.0, 1.0);
        let w = self.width * 0.25;
        self.mics = [-w, w];
        self.reflect = 0.15 + 0.45 * distance.clamp(0.0, 1.0);
        self.extra = (0.0012 + 0.003 * distance.clamp(0.0, 1.0)) * self.sample_rate;
    }

    /// `balance` in [0, 1]: drum only to horn only (0.5: both at full).
    pub fn set_balance(&mut self, balance: f32) {
        let b = balance.clamp(0.0, 1.0);
        self.horn_gain = (2.0 * b).min(1.0);
        self.drum_gain = (2.0 - 2.0 * b).min(1.0);
    }

    /// Delay swing of the horn's path, samples (radius / speed of sound).
    #[cfg(test)]
    pub fn horn_swing(&self) -> f32 {
        self.horn_path.swing
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> (f32, f32) {
        let sr = self.sample_rate;
        self.horn.tick(sr);
        self.drum.tick(sr);
        let (low, high) = self.xover.split(x);
        let high = self.horn_eq.process(high);
        let base = BASE_DELAY * sr;
        #[cfg(test)]
        let reflect = if self.dry_paths { 0.0 } else { self.reflect };
        #[cfg(not(test))]
        let reflect = self.reflect;
        let (hl, hr) = self.horn_path.hear(high, self.horn.angle, self.mics, base, self.extra, reflect);
        // The drum's mics sit lower: a little later, a different angle.
        let drum_mics = [self.mics[0] + 0.1, self.mics[1] + 0.1];
        let (dl, dr) = self.drum_path.hear(low, self.drum.angle, drum_mics, base * 1.3, self.extra * 1.2, reflect);
        // Makeup for the average beam loss: about unity gain on average.
        let norm = 1.0 / (1.0 + 0.5 * reflect);
        let (hg, dg) = (self.horn_gain * HORN_MAKEUP * norm, self.drum_gain * DRUM_MAKEUP * norm);
        (hl * hg + dl * dg, hr * hg + dr * dg)
    }
}

const P_SPEED: u32 = 0;
const P_INERTIA: u32 = 1;
const P_BALANCE: u32 = 2;
const P_DRIVE: u32 = 3;
const P_WIDTH: u32 = 4;
const P_DISTANCE: u32 = 5;
const P_MIX: u32 = 6;

pub(crate) const SPEEDS: &[&str] = &["Stop", "Slow", "Fast"];

static PARAMS: [ParamInfo; 7] = [
    ParamInfo::choice(P_SPEED, "Speed", SPEEDS, 1).group("Rotors"),
    ParamInfo::new(P_INERTIA, "Inertia", 0.25, 4.0, 1.0, Unit::Ratio).log().group("Rotors"),
    ParamInfo::new(P_BALANCE, "Horn/Drum", 0.0, 1.0, 0.5, Unit::Percent).group("Rotors"),
    ParamInfo::new(P_DRIVE, "Drive", 0.0, 1.0, 0.0, Unit::Percent).group("Amp"),
    ParamInfo::new(P_WIDTH, "Mic Width", 0.0, 1.0, 0.7, Unit::Percent).group("Microphones"),
    ParamInfo::new(P_DISTANCE, "Mic Distance", 0.0, 1.0, 0.4, Unit::Percent).group("Microphones"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Microphones"),
];

/// The rotary speaker as an insert effect: the input is summed to mono, as
/// into a real cabinet.
pub struct Rotary {
    values: Values<7>,
    speaker: RotarySpeaker,
    drive: TubeDrive,
    mix: Smoother,
}

impl Default for Rotary {
    fn default() -> Self {
        let mut r = Self {
            values: Values::new(&PARAMS),
            speaker: RotarySpeaker::new(DEFAULT_SR),
            drive: TubeDrive::default(),
            mix: Smoother::new(1.0),
        };
        r.set_sample_rate(DEFAULT_SR);
        r
    }
}

impl Rotary {
    fn update(&mut self) {
        let v = &self.values;
        self.speaker.set_speed(Speed::from_index(v.index(P_SPEED)));
        self.speaker.set_inertia(v.get(P_INERTIA));
        self.speaker.set_balance(v.get(P_BALANCE));
        self.speaker.set_mics(v.get(P_WIDTH), v.get(P_DISTANCE));
        self.mix.set_target(v.get(P_MIX));
    }
}

impl Params for Rotary {
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

impl Effect for Rotary {
    fn kind(&self) -> &'static str {
        "rotary"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.speaker.set_sample_rate(sample_rate);
        self.drive.set_sample_rate(sample_rate);
        self.mix.set_time(0.05, sample_rate);
        self.update();
        self.mix.snap();
    }

    fn reset(&mut self) {
        self.speaker.reset();
        self.drive.reset();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let drive = self.values.get(P_DRIVE);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mono = self.drive.process(0.5 * (*l + *r), drive);
            let (wl, wr) = self.speaker.process(mono);
            let mix = self.mix.next_value();
            *l = *l * (1.0 - mix) + wl * mix;
            *r = *r * (1.0 - mix) + wr * mix;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::effects::testing::*;

    /// Instantaneous frequency of a near-sine around `carrier` Hz: I/Q mix,
    /// one-period moving average, phase difference. One value per sample.
    pub(crate) fn inst_freq(x: &[f32], carrier: f32) -> Vec<f32> {
        let period = (SR / carrier).round() as usize;
        let (mut i_acc, mut q_acc) = (0.0f64, 0.0f64);
        let (mut iq, mut out) = (Vec::new(), Vec::new());
        for (n, &s) in x.iter().enumerate() {
            let ph = TAU as f64 * carrier as f64 * n as f64 / SR as f64;
            iq.push((s as f64 * ph.cos(), -(s as f64) * ph.sin()));
            i_acc += iq[n].0;
            q_acc += iq[n].1;
            if n >= period {
                i_acc -= iq[n - period].0;
                q_acc -= iq[n - period].1;
            }
            out.push(q_acc.atan2(i_acc) as f32);
        }
        let mut f = vec![carrier; x.len()];
        for n in 1..out.len() {
            let mut d = out[n] - out[n - 1];
            while d > std::f32::consts::PI {
                d -= TAU;
            }
            while d < -std::f32::consts::PI {
                d += TAU;
            }
            f[n] = carrier + d * SR / TAU;
        }
        // The image at twice the carrier leaks a fast ripple: average it out
        // over 2 ms (the vibrato itself is far slower).
        let w = (0.002 * SR) as usize;
        (0..f.len()).map(|n| f[n.saturating_sub(w)..=n].iter().sum::<f32>() / (n - n.saturating_sub(w) + 1) as f32).collect()
    }

    fn fast_speaker() -> RotarySpeaker {
        let mut s = RotarySpeaker::new(SR);
        s.dry_paths = true;
        // Pure Doppler: no brightness sweep (a phase shift of its own).
        s.horn_path.beam = false;
        s.set_mics(0.0, 0.0);
        s.set_balance(1.0);
        s.set_speed(Speed::Fast);
        s.horn.speed = HORN_FAST;
        s.drum.speed = DRUM_FAST;
        s
    }

    #[test]
    fn horn_doppler_depth_and_rate_are_a_leslie_s() {
        let mut s = fast_speaker();
        let x = sine(3_000.0, 0.5, 48_000);
        let out: Vec<f32> = x.iter().map(|&v| s.process(v).0).collect();
        let f = inst_freq(&out[4_800..], 3_000.0);
        let f = &f[200..];
        let mut sorted = f.to_vec();
        sorted.sort_by(f32::total_cmp);
        let (lo, hi) = (sorted[sorted.len() / 100], sorted[sorted.len() * 99 / 100]);
        // Expected swing: ±2π r f_rot / c ≈ ±1.8 % at 400 rpm.
        let expect = TAU * HORN_RADIUS * HORN_FAST / SOUND;
        let dev = (hi - lo) * 0.5 / 3_000.0;
        assert!((dev - expect).abs() < 0.25 * expect, "deviation {dev} vs {expect}");
        assert!(dev > 0.012 && dev < 0.025, "{dev}");
        // The vibrato repeats at the horn's rotation rate.
        let mean = f.iter().sum::<f32>() / f.len() as f32;
        let crossings = f.windows(2).filter(|w| w[0] < mean && w[1] >= mean).count() as f32;
        let rate = crossings / (f.len() as f32 / SR);
        assert!((rate - HORN_FAST).abs() < 0.8, "rate {rate}");
        assert!((s.horn_swing() - HORN_RADIUS / SOUND * SR).abs() < 0.01);
    }

    #[test]
    fn horn_amplitude_modulation_is_deep_drum_shallower() {
        let level = |balance: f32, hz: f32| {
            let mut s = fast_speaker();
            s.set_balance(balance);
            let x = sine(hz, 0.5, 48_000);
            let out: Vec<f32> = x.iter().map(|&v| s.process(v).0).collect();
            let win = (SR / hz) as usize * 2;
            let env: Vec<f32> = out[9_600..].chunks(win).map(rms).collect();
            let (lo, hi) = env.iter().fold((f32::MAX, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
            20.0 * (hi / lo).log10()
        };
        let horn = level(1.0, 3_000.0);
        let drum = level(0.0, 200.0);
        assert!(horn > 5.0 && horn < 14.0, "horn AM {horn} dB");
        assert!(drum > 1.5 && drum < horn, "drum AM {drum} dB");
    }

    #[test]
    fn rotors_ramp_with_real_inertia() {
        let mut s = RotarySpeaker::new(SR);
        assert!((s.horn.speed - HORN_SLOW).abs() < 1e-6);
        s.set_speed(Speed::Fast);
        // Time to 90 % of the change, for each rotor.
        let (mut t_horn, mut t_drum) = (None, None);
        for n in 0..(8.0 * SR) as usize {
            s.process(0.0);
            let t = n as f32 / SR;
            if t_horn.is_none() && s.horn.speed >= HORN_SLOW + 0.9 * (HORN_FAST - HORN_SLOW) {
                t_horn = Some(t);
            }
            if t_drum.is_none() && s.drum.speed >= DRUM_SLOW + 0.9 * (DRUM_FAST - DRUM_SLOW) {
                t_drum = Some(t);
            }
        }
        let (h, d) = (t_horn.unwrap(), t_drum.unwrap());
        assert!((0.7..1.6).contains(&h), "horn spin-up {h} s");
        assert!((3.0..6.0).contains(&d), "drum spin-up {d} s");
        // Coasting down is slower than spinning up.
        s.set_speed(Speed::Slow);
        let mut t_down = None;
        for n in 0..(8.0 * SR) as usize {
            s.process(0.0);
            if t_down.is_none() && s.horn.speed <= HORN_FAST - 0.9 * (HORN_FAST - HORN_SLOW) {
                t_down = Some(n as f32 / SR);
            }
        }
        assert!(t_down.unwrap() > h, "{t_down:?}");
        // Brake: everything stops.
        s.set_speed(Speed::Stop);
        for _ in 0..(20.0 * SR) as usize {
            s.process(0.0);
        }
        assert_eq!(s.horn.speed, 0.0);
        assert_eq!(s.drum.speed, 0.0);
    }

    #[test]
    fn stereo_mics_hear_different_things() {
        let mut fx = Rotary::default();
        fx.set_param(P_SPEED, 2.0);
        fx.reset();
        let (l, r) = run_mono(&mut fx, &noise(0.3, 48_000, 3));
        let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        assert!(rms(&diff) > 0.2 * rms(&l), "{} {}", rms(&diff), rms(&l));
        // Mono mics: identical channels.
        fx.set_param(P_WIDTH, 0.0);
        fx.reset();
        let (l, r) = run_mono(&mut fx, &noise(0.3, 9_600, 3));
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-5));
    }

    #[test]
    fn level_is_close_to_unity_and_drive_saturates() {
        let mut fx = Rotary::default();
        let x = sine(440.0, 0.3, 48_000);
        let (l, _) = run_mono(&mut fx, &x);
        let g = rms(&l[9_600..]) / rms(&x[9_600..]);
        assert!(g > 0.5 && g < 1.4, "{g}");
        fx.set_param(P_DRIVE, 1.0);
        let (l, _) = run_mono(&mut fx, &sine(440.0, 1.0, 48_000));
        assert!(peak(&l) < 2.5);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Rotary::default()), 4.0);
    }
}
