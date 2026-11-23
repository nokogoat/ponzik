//! Pitch detection tuned for the voice.
//!
//! The input is high-passed, low-passed and decimated to about 12 kHz, where
//! YIN (de Cheveigné and Kawahara, 2002) runs every 2 ms on a window sized
//! from the lowest note of the input range. The lag found there is refined at
//! the full rate (difference function around it, parabolic interpolation), so
//! the period is known to a fraction of a sample.
//!
//! Octave errors are fought three ways: a lag whose double is a much better
//! match is doubled (strong second harmonic), a lag far longer than a clean
//! half is halved, and during a sung note an estimate an octave off the
//! current period is checked against that period first.
//!
//! The voiced decision needs the periodicity (YIN clarity), the level and a
//! low zero-crossing rate (sibilants cross zero thousands of times a second),
//! with a little hysteresis. Everything is allocated in [`Detector::new`].

use ponzik_dsp::biquad::{BUTTERWORTH4_Q, BiquadCoefs};
use ponzik_dsp::util::midi_to_hz;

/// Input types: name and range of notes (MIDI, lowest and highest).
pub const RANGES: &[&str] = &["Soprano", "Alto", "Tenor", "Baritone", "Bass", "Instrument"];
pub const RANGE_NOTES: [(u8, u8); 6] = [(55, 88), (52, 84), (48, 81), (43, 76), (40, 72), (36, 96)];
/// Lowest note any range reaches (sizes the buffers).
const LOWEST: u8 = 36;
/// Rate of the coarse analysis (about).
const ANALYSIS_RATE: f32 = 12_000.0;
pub const HOP_SECONDS: f32 = 0.002;
const YIN_THRESHOLD: f32 = 0.2;
/// Level under which nothing is voiced (dBFS, RMS of the analysis window).
const GATE_DB: f32 = -58.0;

/// Lowest detectable frequency of a range: a semitone under its lowest note.
pub fn range_floor_hz(range: usize) -> f32 {
    midi_to_hz(f32::from(RANGE_NOTES[range.min(RANGE_NOTES.len() - 1)].0) - 1.0)
}

/// One analysis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Center of the analysed span, in absolute input samples.
    pub time: f64,
    /// Fundamental frequency (0: nothing periodic found).
    pub hz: f32,
    /// Periodicity, 0 (noise) to 1 (pure tone).
    pub clarity: f32,
    pub level_db: f32,
    /// Zero crossings per second of the input around the window.
    pub zcr_hz: f32,
    pub voiced: bool,
}

/// Biquad with its states kept out of denormals.
#[derive(Clone, Copy, Debug)]
struct Sos {
    c: BiquadCoefs,
    z1: f32,
    z2: f32,
}

impl Sos {
    fn new(c: BiquadCoefs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let c = &self.c;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    fn flush(&mut self) {
        if self.z1.abs() < 1e-20 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-20 {
            self.z2 = 0.0;
        }
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

pub struct Detector {
    sample_rate: f32,
    decim: usize,
    rate: f32,
    hp: Sos,
    aa: [Sos; 2],
    phase: usize,
    /// Filtered signal at the full rate (refinement) and decimated (YIN), rings.
    full: Vec<f32>,
    ring: Vec<f32>,
    written: u64,
    dec_written: u64,
    frame: Vec<f32>,
    diff: Vec<f32>,
    cmnd: Vec<f32>,
    window: usize,
    tau_min: usize,
    tau_max: usize,
    hop: usize,
    since_hop: usize,
    /// Zero crossings of the high-passed input, per hop (ring).
    last_sign: bool,
    crossings: u32,
    zc_hops: [u32; 32],
    zc_pos: usize,
    /// Period of the last voiced frame (full rate) and when it was.
    last_period: f32,
    last_voiced_at: u64,
    voiced: bool,
    agree: u32,
    doubt: u32,
    clarity_min: f32,
    low_hz: f32,
    high_hz: f32,
    quality: bool,
}

impl Detector {
    pub fn new(sample_rate: f32) -> Self {
        let sample_rate = sample_rate.max(8_000.0);
        let decim = ((sample_rate / ANALYSIS_RATE).round() as usize).max(1);
        let rate = sample_rate / decim as f32;
        let tau_cap = (rate / midi_to_hz(f32::from(LOWEST) - 1.0)).ceil() as usize + 2;
        // Quality windows are twice the longest period.
        let len = 3 * tau_cap + 8;
        let cutoff = (0.4 * rate).min(3_000.0);
        let mut d = Self {
            sample_rate,
            decim,
            rate,
            hp: Sos::new(BiquadCoefs::highpass(45.0, 0.707, sample_rate)),
            aa: BUTTERWORTH4_Q.map(|q| Sos::new(BiquadCoefs::lowpass(cutoff, q, sample_rate))),
            phase: 0,
            full: vec![0.0; (len * decim).next_power_of_two()],
            ring: vec![0.0; len.next_power_of_two()],
            written: 0,
            dec_written: 0,
            frame: vec![0.0; len],
            diff: vec![0.0; tau_cap + 4],
            cmnd: vec![1.0; tau_cap + 4],
            window: tau_cap,
            tau_min: 2,
            tau_max: tau_cap,
            hop: ((HOP_SECONDS * rate).round() as usize).max(1),
            since_hop: 0,
            last_sign: false,
            crossings: 0,
            zc_hops: [0; 32],
            zc_pos: 0,
            last_period: 0.0,
            last_voiced_at: 0,
            voiced: false,
            agree: 0,
            doubt: 0,
            clarity_min: 0.72,
            low_hz: 100.0,
            high_hz: 1_000.0,
            quality: true,
        };
        d.configure(2, 0.5, true);
        d
    }

    /// Input range, tracking (0 relaxed … 1 strict) and window size (quality: two periods).
    pub fn configure(&mut self, range: usize, tracking: f32, quality: bool) {
        let range = range.min(RANGE_NOTES.len() - 1);
        self.low_hz = range_floor_hz(range);
        self.high_hz = midi_to_hz(f32::from(RANGE_NOTES[range].1) + 1.0).min(0.45 * self.rate);
        self.tau_max = ((self.rate / self.low_hz).ceil() as usize + 1).min(self.diff.len() - 3);
        self.tau_min = ((self.rate / self.high_hz).floor() as usize).max(2);
        let w = if quality { 3 * self.tau_max / 2 } else { self.tau_max.max((0.006 * self.rate) as usize) };
        self.window = w.min(self.frame.len() - self.tau_max - 3);
        self.clarity_min = 0.55 + 0.35 * tracking.clamp(0.0, 1.0);
        self.quality = quality;
    }

    /// Time between two frames, in input samples.
    pub fn hop_samples(&self) -> usize {
        self.hop * self.decim
    }

    /// Longest period the range can have, in input samples.
    pub fn longest_period(&self) -> f32 {
        self.sample_rate / self.low_hz
    }

    /// Input samples from the newest analysed sample back to the center of a frame.
    pub fn span(&self) -> f32 {
        ((self.window + self.tau_max) * self.decim) as f32 * 0.5
    }

    pub fn reset(&mut self) {
        self.hp.reset();
        self.aa.iter_mut().for_each(Sos::reset);
        self.full.fill(0.0);
        self.ring.fill(0.0);
        (self.phase, self.since_hop, self.crossings, self.zc_pos) = (0, 0, 0, 0);
        self.zc_hops = [0; 32];
        self.dec_written = 0;
        self.written = 0;
        self.last_period = 0.0;
        self.voiced = false;
        self.agree = 0;
    }

    /// One input sample; a frame every hop.
    #[inline]
    pub fn push(&mut self, x: f32) -> Option<Frame> {
        let h = self.hp.run(x);
        let sign = h >= 0.0;
        if sign != self.last_sign && h.abs() > 1e-5 {
            self.crossings += 1;
            self.last_sign = sign;
        }
        let mut y = h;
        for f in &mut self.aa {
            y = f.run(y);
        }
        let fm = self.full.len() - 1;
        self.full[self.written as usize & fm] = y;
        self.written += 1;
        self.phase += 1;
        if self.phase < self.decim {
            return None;
        }
        self.phase = 0;
        let rm = self.ring.len() - 1;
        self.ring[self.dec_written as usize & rm] = y;
        self.dec_written += 1;
        self.since_hop += 1;
        if self.since_hop < self.hop {
            return None;
        }
        self.since_hop = 0;
        self.zc_hops[self.zc_pos % 32] = std::mem::take(&mut self.crossings);
        self.zc_pos += 1;
        self.hp.flush();
        self.aa.iter_mut().for_each(Sos::flush);
        Some(self.analyze())
    }

    fn local_min(&self, mut t: usize) -> usize {
        while t < self.tau_max && self.cmnd[t + 1] < self.cmnd[t] {
            t += 1;
        }
        while t > self.tau_min && self.cmnd[t - 1] < self.cmnd[t] {
            t -= 1;
        }
        t
    }

    /// Deepest dip within ±2 lags of `t`.
    fn dip_near(&self, t: usize) -> usize {
        let a = t.saturating_sub(2).max(self.tau_min);
        let b = (t + 2).min(self.tau_max);
        let mut best = a;
        for k in a..=b {
            if self.cmnd[k] < self.cmnd[best] {
                best = k;
            }
        }
        best
    }

    fn analyze(&mut self) -> Frame {
        let (w, tmax, tmin) = (self.window, self.tau_max, self.tau_min);
        let len = w + tmax + 2;
        let center = self.written as f64 - ((w + tmax) * self.decim) as f64 * 0.5;
        let hop_secs = self.hop_samples() as f32 / self.sample_rate;
        let span_hops = (((w + tmax) * self.decim) as f32 / self.hop_samples() as f32).ceil() as usize;
        let span_hops = span_hops.clamp(1, 32).min(self.zc_pos.max(1));
        let crossings: u32 = (0..span_hops).map(|k| self.zc_hops[(self.zc_pos + 32 - 1 - k) % 32]).sum();
        let zcr_hz = crossings as f32 / (span_hops as f32 * hop_secs);
        let mut out = Frame { time: center, hz: 0.0, clarity: 0.0, level_db: -120.0, zcr_hz, voiced: false };
        if (self.dec_written as usize) < len || tmin + 2 >= tmax {
            self.voiced = false;
            return out;
        }
        let rm = self.ring.len() - 1;
        let start = self.dec_written as usize - len;
        for (k, dst) in self.frame[..len].iter_mut().enumerate() {
            *dst = self.ring[(start + k) & rm];
        }
        let f = &self.frame[..len];
        let end = len - 1;
        let energy = f[len - w..].iter().map(|x| x * x).sum::<f32>() / w as f32;
        out.level_db = 10.0 * (energy + 1e-12).log10();
        if out.level_db < GATE_DB - 12.0 {
            self.voiced = false;
            self.agree = 0;
            return out;
        }
        for tau in 1..=tmax + 1 {
            let mut d = 0.0;
            for j in 0..w {
                let a = f[end - j] - f[end - j - tau];
                d += a * a;
            }
            self.diff[tau] = d;
        }
        self.cmnd[0] = 1.0;
        let mut running = 0.0;
        for tau in 1..=tmax + 1 {
            running += self.diff[tau];
            self.cmnd[tau] = if running > 1e-12 { self.diff[tau] * tau as f32 / running } else { 1.0 };
        }
        let mut tau = match (tmin..=tmax).find(|&t| self.cmnd[t] < YIN_THRESHOLD) {
            Some(t) => self.local_min(t),
            None => (tmin..=tmax).min_by(|&a, &b| self.cmnd[a].total_cmp(&self.cmnd[b])).unwrap_or(tmin),
        };
        // Strong second harmonic: the double lag matches much better.
        if 2 * tau + 2 <= tmax {
            let t2 = self.dip_near(2 * tau);
            if self.cmnd[t2] < self.cmnd[tau] - 0.1 {
                tau = t2;
            }
        }
        // A sung note: an estimate an octave off the current period is checked against it.
        let recent = self.written.saturating_sub(self.last_voiced_at) < (0.06 * self.sample_rate) as u64;
        if self.last_period > 0.0 && recent {
            let ratio = (tau * self.decim) as f32 / self.last_period;
            let off = (ratio - 0.5).abs() < 0.06 || (ratio - 2.0).abs() < 0.16 || (ratio - 1.0 / 3.0).abs() < 0.04;
            let near = (self.last_period / self.decim as f32).round() as usize;
            if off && near > tmin && near < tmax {
                let alt = self.dip_near(near);
                if self.cmnd[alt] < (self.cmnd[tau] + 0.12).min(0.45) {
                    tau = alt;
                }
            }
        }
        let clarity = (1.0 - self.cmnd[tau]).clamp(0.0, 1.0);
        out.clarity = clarity;
        if tau <= 1 || tau > tmax {
            self.voiced = false;
            return out;
        }
        let (a, b, c) = (self.cmnd[tau - 1], self.cmnd[tau], self.cmnd[tau + 1]);
        let den = a - 2.0 * b + c;
        let shift = if den > 0.0 { (0.5 * (a - c) / den).clamp(-1.0, 1.0) } else { 0.0 };
        let period = self.refine((tau as f32 + shift) * self.decim as f32, w * self.decim);
        let hz = self.sample_rate / period;
        out.hz = hz;
        // The window and the lagged copy span W + period, ending now.
        out.time = self.written as f64 - ((w * self.decim) as f64 + f64::from(period)) * 0.5;
        let raw = clarity >= self.clarity_min
            && out.level_db >= GATE_DB
            && zcr_hz < (3_500.0f32).max(10.0 * hz)
            && hz >= self.low_hz * 0.97
            && hz <= self.high_hz * 1.03;
        if raw {
            self.agree += 1;
            self.doubt = 0;
            if self.agree >= if clarity > 0.88 { 1 } else { 2 } {
                self.voiced = true;
            }
        } else {
            self.agree = 0;
            self.doubt += 1;
            // One doubtful frame inside a voiced part is bridged, two end it.
            if self.doubt >= 2 || out.level_db < GATE_DB || clarity < 0.4 {
                self.voiced = false;
            } else if self.last_period > 0.0 {
                out.hz = self.sample_rate / self.last_period;
            }
        }
        if self.voiced {
            if raw {
                self.last_period = period;
            }
            self.last_voiced_at = self.written;
        }
        out.voiced = self.voiced;
        out
    }

    /// Period refined at the full rate around `coarse` (full-rate samples).
    fn refine(&self, coarse: f32, window: usize) -> f32 {
        let m = self.full.len() - 1;
        let end = self.written as usize - 1;
        let d = self.decim as i64;
        let w = window.min(1_024);
        let center = coarse.round() as i64;
        let lo = (center - d).max(2);
        let hi = center + d;
        if self.decim == 1 || (hi as usize + w + 2) > self.full.len() {
            return coarse;
        }
        let diff = |lag: i64| -> f32 {
            let lag = lag as usize;
            let mut s = 0.0;
            for j in 0..w {
                let a = self.full[(end - j) & m] - self.full[(end - j - lag) & m];
                s += a * a;
            }
            s
        };
        let mut best = (lo, f32::MAX);
        for lag in lo..=hi {
            let v = diff(lag);
            if v < best.1 {
                best = (lag, v);
            }
        }
        let (a, b, c) = (diff(best.0 - 1), best.1, diff(best.0 + 1));
        let den = a - 2.0 * b + c;
        let shift = if den > 0.0 { (0.5 * (a - c) / den).clamp(-1.0, 1.0) } else { 0.0 };
        best.0 as f32 + shift
    }
}
