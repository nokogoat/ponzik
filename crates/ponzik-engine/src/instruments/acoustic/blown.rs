//! Blown instruments: one model, four mechanisms.
//!
//! - Lips (trumpet, trombone, horn, tuba): the lips are a pressure
//!   controlled valve near the narrow end of a cone (two delay lines split
//!   at the lips), tongued open at the attack. The bell closes the bore:
//!   lows reflect (lowpass in the loop), highs radiate (its complement).
//!   Loud notes steepen into "brassy" shock waves: the radiated wave is
//!   read through a delay that shortens with its own amplitude.
//! - Jet (flute): the breath jet crosses the embouchure (a short delay),
//!   is deflected by the bore's flow through a cubic, and drives the bore.
//! - Cylindrical reed (clarinet): a reed table (pressure-controlled valve)
//!   on a bore half a period long: odd harmonics.
//! - Conical reed (saxophone, oboe): the reed sits between two delay lines
//!   (a cone split at the reed), all harmonics.
//!
//! Breath noise rides on the pressure; the tuning compensates the phase
//! delay of the loop filters at the note.

use super::section::Model;
use super::wg::{Delay, Loss};
use ponzik_dsp::util::flush_denormal;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mechanism {
    Lips,
    Jet,
    Clarinet,
    Conical,
}

/// One instrument's constants.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spec {
    pub mechanism: Mechanism,
    /// Loop loss filter: gain and pole (the bell or the bore's walls).
    pub loss: (f32, f32),
    /// Breath pressure at pianissimo and fortissimo.
    pub pressure: (f32, f32),
    /// Reed split position (conical), jet ratio (flute) or lip radius (lips).
    pub shape: f32,
    /// Reed table slope (reeds).
    pub slope: f32,
    /// Output gain.
    pub gain: f32,
    /// Tuning correction in cents (the nonlinear drive pulls the pitch).
    pub cents: f32,
}

/// Settings read by the model.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BlowCfg {
    pub spec: Spec,
    /// Shock-wave steepening, [0, 1].
    pub brassiness: f32,
    /// Darker (0) or brighter (1) bell / bore.
    pub brightness: f32,
    /// Breath noise, [0, 1].
    pub noise: f32,
}

pub(crate) struct Blown {
    sr: f32,
    bore: Delay,
    /// Second line: the jet (flute) or the cone's short side (conical).
    aux: Delay,
    d: [f32; 2],
    step: [f32; 2],
    loss: Loss,
    dc: (f32, f32),
    shock: Delay,
    /// One-zero state (reeds).
    z1: f32,
    dynamics: f32,
    /// Output tone: soft playing is darker and quieter.
    tone: f32,
    tone_y: f32,
    hz: f32,
    level: f32,
    /// Tonguing: a pressure kick at the attack that starts the lips.
    tongue: f32,
}

#[inline]
fn reed_table(x: f32, slope: f32) -> f32 {
    (0.7 + slope * x).clamp(-1.0, 1.0)
}

impl Model for Blown {
    type Cfg = BlowCfg;

    fn new(sample_rate: f32, min_hz: f32, _seed: u32) -> Self {
        // Lips use a bore two periods long.
        let max = (2.0 * sample_rate / min_hz) as usize + 8;
        Self {
            sr: sample_rate,
            bore: Delay::new(max),
            aux: Delay::new(max / 2),
            d: [4.0; 2],
            step: [0.0; 2],
            loss: Loss::default(),
            dc: (0.0, 0.0),
            shock: Delay::new(64),
            z1: 0.0,
            dynamics: 0.0,
            tone: 1.0,
            tone_y: 0.0,
            hz: 220.0,
            level: 0.0,
            tongue: 0.0,
        }
    }

    fn reset(&mut self) {
        self.bore.clear();
        self.aux.clear();
        self.shock.clear();
        self.loss.reset();
        self.dc = (0.0, 0.0);
        self.z1 = 0.0;
        self.level = 0.0;
        self.tone_y = 0.0;

    }

    fn attack(&mut self, _cfg: &BlowCfg, velocity: f32, _pluck: bool) {
        self.tongue = 0.5 * velocity;
    }

    fn tune(&mut self, cfg: &BlowCfg, hz: f32, dynamics: f32, _released: bool, n: usize, fresh: bool) {
        let sr = self.sr;
        let s = &cfg.spec;
        let hz = hz * (s.cents / 1_200.0).exp2();
        self.hz = hz;
        self.dynamics = dynamics.max(0.02);
        let pole = (s.loss.1 + 0.2 * (0.5 - cfg.brightness)).clamp(0.0, 0.95);
        self.loss.set(s.loss.0, pole);
        let d = self.dynamics.min(1.0);
        let cutoff = hz * (2.5 + 22.0 * d * d) * (0.6 + 0.8 * cfg.brightness);
        self.tone = 1.0 - (-std::f32::consts::TAU * cutoff.min(0.45 * sr) / sr).exp();
        let period = sr / hz;
        let lp = self.loss.delay(hz, sr);
        let target = match s.mechanism {
            Mechanism::Lips => {
                // The lips sit near the end of a cone closed by the bell.
                let total = period - lp - 0.5;
                [total * (1.0 - s.shape), total * s.shape]
            }
            Mechanism::Jet => {
                // Overblown: the bore is one and a half periods long.
                let bore = 1.5 * period - lowpass_phase(pole, hz / 1.5, sr);
                [bore, bore * s.shape]
            }
            Mechanism::Clarinet => [0.5 * period - 0.5, 0.0],
            Mechanism::Conical => {
                let total = period - 0.5;
                [total * (1.0 - s.shape), total * s.shape]
            }
        };
        for ((d, step), t) in self.d.iter_mut().zip(&mut self.step).zip(target) {
            let t = t.max(2.0);
            if fresh {
                *d = t;
                *step = 0.0;
            } else {
                *step = (t - *d) / n as f32;
            }
        }
    }

    #[inline]
    fn tick(&mut self, cfg: &BlowCfg, drive: f32, noise: f32) -> f32 {
        let s = &cfg.spec;
        self.d[0] += self.step[0];
        self.d[1] += self.step[1];
        let env = (drive.abs() / self.dynamics).min(1.0);
        let pressure = s.pressure.0 + (s.pressure.1 - s.pressure.0) * self.dynamics.min(1.0);
        let breath = pressure * env * (1.0 + cfg.noise * 0.12 * noise);
        let out = match s.mechanism {
            Mechanism::Lips => {
                let long = self.bore.read(self.d[0]);
                let short = self.aux.read(self.d[1]);
                // The bell reflects the lows back into the bore.
                let smooth = 0.5 * (long + self.z1);
                self.z1 = long;
                let refl = self.loss.process(smooth);
                let temp = -refl;
                let last = temp - short;
                // Tonguing kicks the lips open at the attack.
                let pd = breath * (1.0 + self.tongue) - last;
                self.tongue *= 0.99;
                self.aux.write(flush_denormal(temp));
                self.bore.write(flush_denormal(breath - pd * reed_table(pd, s.slope) - temp));
                // Radiated: what the bell lets through (its highs) over the body of the tone.
                let radiated = last + 0.25 * (long - refl / s.loss.0.max(0.1));
                // Shock waves: louder parts travel faster.
                self.shock.write(radiated);
                let k = cfg.brassiness * 14.0 * self.dynamics.min(1.0);
                self.shock.read((20.0 - k * radiated.clamp(-1.0, 1.0)).clamp(4.0, 40.0))
            }
            Mechanism::Jet => {
                let bore_out = self.bore.read(self.d[0]);
                let temp = -self.loss.process(bore_out);
                let blocked = temp - self.dc.0 + 0.995 * self.dc.1;
                self.dc = (temp, flush_denormal(blocked));
                let pd = breath - 0.5 * blocked;
                let jet = self.aux.read(self.d[1]);
                self.aux.write(pd);
                let jt = (jet * (jet * jet - 1.0)).clamp(-1.0, 1.0);
                self.bore.write(flush_denormal(jt + 0.5 * blocked));
                bore_out
            }
            Mechanism::Clarinet => {
                let bore_out = self.bore.read(self.d[0]);
                let f = 0.5 * (bore_out + self.z1);
                self.z1 = bore_out;
                let pd = -s.loss.0 * f - breath;
                self.bore.write(flush_denormal(breath + pd * reed_table(pd, s.slope)));
                bore_out
            }
            Mechanism::Conical => {
                let long = self.bore.read(self.d[0]);
                let short = self.aux.read(self.d[1]);
                let f = 0.5 * (long + self.z1);
                self.z1 = long;
                let temp = -s.loss.0 * f;
                let last = temp - short;
                let pd = breath - last;
                self.aux.write(flush_denormal(temp));
                self.bore.write(flush_denormal(breath - pd * reed_table(pd, s.slope) - temp));
                last
            }
        };
        self.tone_y = flush_denormal(self.tone_y + (out - self.tone_y) * self.tone);
        self.tone_y * s.gain * (0.45 + 0.55 * self.dynamics.min(1.0))
    }
}

/// Phase delay (samples) of the one-pole lowpass with pole `a` at `hz`.
fn lowpass_phase(a: f32, hz: f32, sr: f32) -> f32 {
    super::wg::lowpass_delay(a, hz, sr)
}
