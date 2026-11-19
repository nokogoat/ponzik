//! Voice to notes: real-time monophonic pitch tracking that turns a sung or
//! hummed line into note events.
//!
//! The input is high-passed, low-passed and decimated to about 12 kHz, where a
//! YIN difference function (de Cheveigné and Kawahara, 2002) runs every hop
//! (about 2.5 ms) on a window sized from the lowest note of the range: two
//! periods of it, so the lower the range, the longer the window. The lag found
//! there is refined at the full rate. A note starts once two hops agree on a
//! confident pitch above the gate; it changes only when the voice moves past
//! the next note by a hysteresis margin for long enough (vibrato of ±50 cents
//! never flutters), and ends when the voice stops. Octave jumps of the
//! estimate during a note are checked against the current period. Energy
//! onsets re-articulate a repeated note ("da da da" on one pitch).
//!
//! The tracker allocates in [`VoiceTracker::new`] only: [`VoiceTracker::process`]
//! runs on the audio thread. [`transcribe`] is the offline variant (it
//! allocates its result): a recorded clip into notes, with the same analysis.

use crate::biquad::{BUTTERWORTH4_Q, Biquad, BiquadCoefs};
use crate::util::midi_to_hz;

/// Lowest note the tracker can follow (C2, 65 Hz).
pub const LOWEST: u8 = 36;
/// Highest note the tracker can follow (C7, 2093 Hz).
pub const HIGHEST: u8 = 96;
/// Rate of the analysis after decimation (about).
const ANALYSIS_RATE: f32 = 12_000.0;
/// Time between two analyses.
const HOP_SECONDS: f32 = 0.0025;
/// YIN's absolute threshold: the first dip of the normalized difference under it is the period.
const YIN_THRESHOLD: f32 = 0.2;
/// The voice must move this far (semitones) closer to another note than to
/// the sounding one before the note changes.
const HYSTERESIS: f32 = 0.3;
/// A small move must hold this long to change the note; a leap of
/// [`LEAP`] semitones or more changes it after two hops.
const CHANGE_SECONDS: f32 = 0.03;
const LEAP: f32 = 1.5;
/// The note ends this long after the level falls under the gate, or after
/// the voice turns unpitched (breath, consonant) while staying loud.
const RELEASE_QUIET_SECONDS: f32 = 0.012;
const RELEASE_UNVOICED_SECONDS: f32 = 0.05;
/// A rise of the level by this much over its recent low is an attack.
const ONSET_DB: f32 = 9.0;
/// The recent low of the level climbs back by this much per second.
const VALLEY_RISE_DB_PER_SEC: f32 = 120.0;
/// The gate closes this much below where it opens.
const GATE_HYSTERESIS_DB: f32 = 6.0;

/// How the detected notes are played.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VoiceMode {
    /// A note per sung note: each change of pitch and each new attack retriggers.
    #[default]
    Normal,
    /// Pitch changes overlap the next note with the previous one (synths with
    /// portamento glide), repeated attacks on one pitch don't retrigger.
    Legato,
    /// The last note keeps sounding through the silences, until the next one.
    Hold,
}

impl VoiceMode {
    pub const ALL: [VoiceMode; 3] = [Self::Normal, Self::Legato, Self::Hold];

    pub fn id(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Legato => "legato",
            Self::Hold => "hold",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// Settings of the tracker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceConfig {
    /// Range of the voice (MIDI notes, within [`LOWEST`]..=[`HIGHEST`]):
    /// pitches outside are ignored. The lowest note sets the analysis window,
    /// hence the reaction time.
    pub low: u8,
    pub high: u8,
    /// Level under which nothing is detected (dBFS, RMS).
    pub gate_db: f32,
    /// Periodicity needed to call the input pitched, 0 to 1.
    pub confidence: f32,
    /// Allowed pitch classes (bit 0: C … bit 11: B); 0 or `0xFFF`: chromatic.
    pub scale: u16,
    /// Semitones added to the voice before snapping.
    pub transpose: i8,
    /// 0: notes snap hard to their key; 1: the played note follows the
    /// voice's bends and vibrato through [`VoiceEvent::Bend`].
    pub retune: f32,
    /// Velocity from the voice's loudness: 0 (always 0.8) to 1 (full range).
    pub velocity: f32,
    pub mode: VoiceMode,
    /// Shortest note, in seconds: a note does not change or end sooner.
    pub min_note: f32,
    /// Frequency of A4.
    pub reference_hz: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            low: 40,
            high: 84,
            gate_db: -50.0,
            confidence: 0.6,
            scale: 0xFFF,
            transpose: 0,
            retune: 0.0,
            velocity: 0.7,
            mode: VoiceMode::Normal,
            min_note: 0.05,
            reference_hz: 440.0,
        }
    }
}

/// One analysis of the input, every hop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceFrame {
    /// Pitch of the voice in MIDI notes (fractional, before transposition);
    /// NaN when the input is not pitched.
    pub pitch: f32,
    /// Periodicity, 0 (noise) to 1 (pure tone).
    pub clarity: f32,
    /// RMS level, dBFS.
    pub level_db: f32,
    /// The note being played.
    pub note: Option<u8>,
}

/// What the tracker plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceEvent {
    /// A note starts; the voice's attack was `onset` samples earlier (the
    /// reaction time), to place a recorded note where it was sung.
    NoteOn { key: u8, velocity: f32, onset: u32 },
    NoteOff { key: u8 },
    /// Pitch of the voice relative to the sounding `key`, in semitones,
    /// scaled by [`VoiceConfig::retune`].
    Bend { key: u8, semitones: f32 },
    Frame(VoiceFrame),
}

/// The note played, and when it started (input samples).
#[derive(Clone, Copy, Debug)]
struct Sounding {
    key: u8,
    start: u64,
    /// Hold mode: the voice stopped, the note keeps sounding until the next one.
    resting: bool,
}

/// Nearest note of `mask` (bit 0: C … bit 11: B) to the fractional MIDI pitch
/// `pitch`, the lower one on a tie; 0 or `0xFFF`: nearest semitone.
pub fn snap_to_scale(pitch: f32, mask: u16) -> u8 {
    let mask = if mask & 0xFFF == 0 { 0xFFF } else { mask & 0xFFF };
    let base = pitch.floor() as i32;
    let mut best = (f32::MAX, base);
    for n in base - 6..=base + 7 {
        if mask & (1 << n.rem_euclid(12)) == 0 {
            continue;
        }
        let d = (pitch - n as f32).abs();
        // Ties go to the lower note (`n` increases).
        if d < best.0 - 1e-4 {
            best = (d, n);
        }
    }
    best.1.clamp(0, 127) as u8
}

/// Fractional MIDI note of a frequency.
pub fn hz_to_midi(hz: f32, reference_hz: f32) -> f32 {
    69.0 + 12.0 * (hz / reference_hz).log2()
}

/// Real-time monophonic pitch tracker and note decider.
#[derive(Clone, Debug)]
pub struct VoiceTracker {
    config: VoiceConfig,
    sample_rate: f32,
    /// Decimation factor and the analysis rate it gives.
    decim: usize,
    rate: f32,
    high_pass: Biquad,
    anti_alias: [Biquad; 2],
    phase: usize,
    /// Decimated signal (ring), and the full-rate filtered one for the refinement.
    ring: Vec<f32>,
    pos: usize,
    full: Vec<f32>,
    full_pos: usize,
    filled: usize,
    frame: Vec<f32>,
    full_frame: Vec<f32>,
    diff: Vec<f32>,
    cmnd: Vec<f32>,
    /// Analysis window and lag range, at the analysis rate.
    window: usize,
    tau_min: usize,
    tau_max: usize,
    hop: usize,
    since_hop: usize,
    /// Mean square of the input (fast follower) and its coefficients.
    env: f32,
    env_attack: f32,
    env_release: f32,
    valley_db: f32,
    /// Input samples seen.
    clock: u64,
    /// Where the current attack started (input samples).
    onset_at: Option<u64>,
    sounding: Option<Sounding>,
    /// Consecutive pitched hops agreeing with each other, and the last pitch.
    stable: u32,
    last_pitch: f32,
    /// Period of the voice (analysis samples) while a note plays, for octave checks.
    period: f32,
    /// Note the voice is moving to, and for how many hops.
    candidate: Option<u8>,
    candidate_hops: u32,
    /// Unpitched or quiet hops in a row.
    quiet_hops: u32,
    last_bend: f32,
    /// Last reaction time: attack to note start, in seconds.
    reaction: f32,
}

impl VoiceTracker {
    pub fn new(sample_rate: f32, config: VoiceConfig) -> Self {
        let sample_rate = sample_rate.max(8_000.0);
        let decim = ((sample_rate / ANALYSIS_RATE).round() as usize).max(1);
        let rate = sample_rate / decim as f32;
        // Sized for the lowest note the tracker can follow.
        let (tau_cap, window_cap) = Self::lags(rate, LOWEST);
        let len = window_cap + tau_cap + 2;
        let ms = |s: f32| (-1.0 / (s * sample_rate)).exp();
        let cutoff = (rate * 0.4).min(5_000.0);
        let mut t = Self {
            config,
            sample_rate,
            decim,
            rate,
            high_pass: Biquad::new(BiquadCoefs::highpass(45.0, 0.707, sample_rate)),
            anti_alias: BUTTERWORTH4_Q.map(|q| Biquad::new(BiquadCoefs::lowpass(cutoff, q, sample_rate))),
            phase: 0,
            ring: vec![0.0; len],
            pos: 0,
            full: vec![0.0; len * decim],
            full_pos: 0,
            filled: 0,
            frame: vec![0.0; len],
            full_frame: vec![0.0; len * decim],
            diff: vec![0.0; tau_cap + 2],
            cmnd: vec![1.0; tau_cap + 2],
            window: window_cap,
            tau_min: 2,
            tau_max: tau_cap,
            hop: ((HOP_SECONDS * rate).round() as usize).max(1),
            since_hop: 0,
            env: 0.0,
            env_attack: 1.0 - ms(0.002),
            env_release: 1.0 - ms(0.01),
            valley_db: -120.0,
            clock: 0,
            onset_at: None,
            sounding: None,
            stable: 0,
            last_pitch: f32::NAN,
            period: 0.0,
            candidate: None,
            candidate_hops: 0,
            quiet_hops: 0,
            last_bend: 0.0,
            reaction: 0.0,
        };
        t.set_config(config);
        t
    }

    /// Largest lag and window for a range starting at `low`, at `rate`.
    fn lags(rate: f32, low: u8) -> (usize, usize) {
        let tau_max = (rate / midi_to_hz(f32::from(low.max(LOWEST)) - 1.0)).ceil() as usize + 1;
        (tau_max, tau_max.max((0.008 * rate) as usize))
    }

    pub fn config(&self) -> &VoiceConfig {
        &self.config
    }

    /// New settings; a range change applies to the next analysis.
    pub fn set_config(&mut self, config: VoiceConfig) {
        let mut c = config;
        c.low = c.low.clamp(LOWEST, HIGHEST - 1);
        c.high = c.high.clamp(c.low + 1, HIGHEST);
        c.confidence = c.confidence.clamp(0.0, 1.0);
        c.retune = c.retune.clamp(0.0, 1.0);
        c.velocity = c.velocity.clamp(0.0, 1.0);
        c.transpose = c.transpose.clamp(-48, 48);
        c.min_note = c.min_note.clamp(0.0, 1.0);
        c.reference_hz = c.reference_hz.clamp(400.0, 480.0);
        let (tau_max, window) = Self::lags(self.rate, c.low);
        self.tau_max = tau_max.min(self.diff.len() - 2);
        self.window = window.min(self.ring.len() - self.tau_max - 2);
        self.tau_min = ((self.rate / midi_to_hz(f32::from(c.high) + 1.0)).floor() as usize).max(2);
        self.config = c;
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Time between two analyses, in input samples.
    pub fn hop_samples(&self) -> usize {
        self.hop * self.decim
    }

    /// Reaction time of the last note started from silence: from the voice's
    /// attack to the note, in seconds.
    pub fn reaction(&self) -> f32 {
        self.reaction
    }

    /// Note playing now.
    pub fn sounding(&self) -> Option<u8> {
        self.sounding.map(|s| s.key)
    }

    /// Forgets everything (no event: see [`VoiceTracker::release`]).
    pub fn reset(&mut self) {
        self.high_pass.reset();
        for f in &mut self.anti_alias {
            f.reset();
        }
        self.ring.fill(0.0);
        self.full.fill(0.0);
        (self.phase, self.pos, self.full_pos, self.filled, self.since_hop) = (0, 0, 0, 0, 0);
        self.env = 0.0;
        self.valley_db = -120.0;
        self.onset_at = None;
        self.sounding = None;
        self.stable = 0;
        self.last_pitch = f32::NAN;
        self.period = 0.0;
        self.candidate = None;
        self.candidate_hops = 0;
        self.quiet_hops = 0;
        self.last_bend = 0.0;
    }

    /// Ends the note playing, if any (input closed, tracker switched off).
    pub fn release(&mut self, emit: &mut impl FnMut(usize, VoiceEvent)) {
        if let Some(s) = self.sounding.take() {
            emit(0, VoiceEvent::NoteOff { key: s.key });
        }
        self.candidate = None;
        self.stable = 0;
    }

    /// Feeds mono input; events come with their sample in `input`.
    pub fn process(&mut self, input: &[f32], emit: &mut impl FnMut(usize, VoiceEvent)) {
        let ring_len = self.ring.len();
        let full_len = self.full.len();
        for (i, &x) in input.iter().enumerate() {
            let x = if x.is_finite() { x } else { 0.0 };
            let mut y = self.high_pass.process(x);
            for f in &mut self.anti_alias {
                y = f.process(y);
            }
            self.full[self.full_pos] = y;
            self.full_pos = (self.full_pos + 1) % full_len;
            let e = y * y;
            let k = if e > self.env { self.env_attack } else { self.env_release };
            self.env += (e - self.env) * k;
            self.clock += 1;
            self.phase += 1;
            if self.phase < self.decim {
                continue;
            }
            self.phase = 0;
            self.ring[self.pos] = y;
            self.pos = (self.pos + 1) % ring_len;
            self.filled = (self.filled + 1).min(ring_len);
            self.since_hop += 1;
            if self.since_hop >= self.hop {
                self.since_hop = 0;
                self.analyze(i, emit);
            }
        }
        // Keeps the followers out of denormals.
        if self.env < 1e-20 {
            self.env = 0.0;
        }
    }

    /// One hop: level, attack, pitch, then the note decision.
    fn analyze(&mut self, at: usize, emit: &mut impl FnMut(usize, VoiceEvent)) {
        let level_db = 10.0 * (self.env + 1e-12).log10();
        let hop_secs = self.hop as f32 / self.rate;
        self.valley_db = (self.valley_db + VALLEY_RISE_DB_PER_SEC * hop_secs).min(level_db);
        let gate = self.config.gate_db - if self.sounding.is_some_and(|s| !s.resting) { GATE_HYSTERESIS_DB } else { 0.0 };
        let mut attack = false;
        if level_db >= self.config.gate_db && level_db - self.valley_db > ONSET_DB {
            attack = true;
            // The attack started when the level left its low, a few hops ago
            // at most: the follower's own rise is counted in.
            let rise = ((level_db - self.valley_db) / 60.0 * 0.004 * self.sample_rate) as u64;
            self.onset_at = Some(self.clock.saturating_sub(rise.min((0.006 * self.sample_rate) as u64)));
            self.valley_db = level_db;
        }
        let detected = if level_db >= gate - 10.0 { self.detect() } else { None };
        let (pitch, clarity) = detected.unwrap_or((f32::NAN, 0.0));
        let c = self.config;
        let voiced = detected.is_some()
            && clarity >= c.confidence
            && level_db >= gate
            && pitch >= f32::from(c.low) - 0.5
            && pitch <= f32::from(c.high) + 0.5;
        self.decide(at, voiced, pitch, level_db, attack, emit);
        let shown = if detected.is_some() && level_db >= gate { pitch } else { f32::NAN };
        emit(at, VoiceEvent::Frame(VoiceFrame { pitch: shown, clarity, level_db, note: self.sounding.map(|s| s.key) }));
    }

    /// YIN on the window ending now: (MIDI pitch, clarity), or `None` while
    /// the window fills or when nothing periodic is found.
    fn detect(&mut self) -> Option<(f32, f32)> {
        let (w, tau_min, tau_max) = (self.window, self.tau_min, self.tau_max);
        let len = w + tau_max + 2;
        if self.filled < len || tau_min + 2 >= tau_max {
            return None;
        }
        // The last `len` samples, oldest first.
        let n = self.ring.len();
        for (k, dst) in self.frame[..len].iter_mut().enumerate() {
            *dst = self.ring[(self.pos + n - len + k) % n];
        }
        let f = &self.frame[..len];
        let end = len - 1;
        // Difference over the newest `w` samples against `tau` earlier.
        for tau in 1..=tau_max + 1 {
            let mut d = 0.0;
            for j in 0..w {
                let a = f[end - j] - f[end - j - tau];
                d += a * a;
            }
            self.diff[tau] = d;
        }
        self.cmnd[0] = 1.0;
        let mut running = 0.0;
        for tau in 1..=tau_max + 1 {
            running += self.diff[tau];
            self.cmnd[tau] = if running > 1e-12 { self.diff[tau] * tau as f32 / running } else { 1.0 };
        }
        let local_min = |cmnd: &[f32], mut t: usize| {
            while t < tau_max && cmnd[t + 1] < cmnd[t] {
                t += 1;
            }
            while t > tau_min && cmnd[t - 1] < cmnd[t] {
                t -= 1;
            }
            t
        };
        // First dip under the threshold, else the deepest one.
        let mut tau = match (tau_min..=tau_max).find(|&t| self.cmnd[t] < YIN_THRESHOLD) {
            Some(t) => local_min(&self.cmnd, t),
            None => (tau_min..=tau_max).min_by(|&a, &b| self.cmnd[a].total_cmp(&self.cmnd[b]))?,
        };
        // During a note, an estimate an octave off the current period is
        // checked against it: kept only if clearly better.
        if self.period > 0.0 && self.sounding.is_some() {
            let ratio = tau as f32 / self.period;
            let off = (ratio - 0.5).abs() < 0.06 || (ratio - 2.0).abs() < 0.2 || (ratio - 1.0 / 3.0).abs() < 0.04;
            let near = self.period.round() as usize;
            if off && near > tau_min && near < tau_max {
                let alt = local_min(&self.cmnd, near);
                if self.cmnd[alt] < (self.cmnd[tau] + 0.12).min(0.5) {
                    tau = alt;
                }
            }
        }
        let clarity = (1.0 - self.cmnd[tau]).clamp(0.0, 1.0);
        if tau <= 1 || tau > tau_max {
            return None;
        }
        let (a, b, c) = (self.cmnd[tau - 1], self.cmnd[tau], self.cmnd[tau + 1]);
        let den = a - 2.0 * b + c;
        let shift = if den > 0.0 { (0.5 * (a - c) / den).clamp(-1.0, 1.0) } else { 0.0 };
        let mut lag = (tau as f32 + shift) * self.decim as f32;
        if self.decim > 1 {
            lag = self.refine(lag, w * self.decim);
        }
        let hz = self.sample_rate / lag;
        self.period = tau as f32 + shift;
        Some((hz_to_midi(hz, self.config.reference_hz), clarity))
    }

    /// Lag of the minimum of the full-rate difference near `lag`, interpolated.
    fn refine(&mut self, lag: f32, w: usize) -> f32 {
        let n = self.full.len();
        let center = lag.round() as usize;
        let reach = self.decim + 1;
        let (lo, hi) = (center.saturating_sub(reach).max(2), center + reach);
        let len = w + hi + 2;
        if len > n {
            return lag;
        }
        for (k, dst) in self.full_frame[..len].iter_mut().enumerate() {
            *dst = self.full[(self.full_pos + n - len + k) % n];
        }
        let f = &self.full_frame[..len];
        let end = len - 1;
        let d = |tau: usize| (0..w).map(|j| f[end - j] - f[end - j - tau]).map(|a| a * a).sum::<f32>();
        let mut best = (f32::MAX, center);
        for tau in lo..=hi {
            let v = d(tau);
            if v < best.0 {
                best = (v, tau);
            }
        }
        let t = best.1;
        if t <= lo || t >= hi {
            return lag;
        }
        let (a, b, c) = (d(t - 1), best.0, d(t + 1));
        let den = a - 2.0 * b + c;
        let shift = if den > 0.0 { (0.5 * (a - c) / den).clamp(-1.0, 1.0) } else { 0.0 };
        t as f32 + shift
    }

    fn velocity(&self, level_db: f32) -> f32 {
        let x = ((level_db - (self.config.gate_db + 6.0)) / 30.0).clamp(0.0, 1.0);
        let raw = 0.3 + 0.7 * x;
        (0.8 + (raw - 0.8) * self.config.velocity).clamp(0.05, 1.0)
    }

    fn hops(&self, secs: f32) -> u32 {
        ((secs * self.rate / self.hop as f32).ceil() as u32).max(1)
    }

    fn start(&mut self, at: usize, key: u8, level_db: f32, emit: &mut impl FnMut(usize, VoiceEvent)) {
        let onset = self.onset_at.map_or(0, |o| self.clock.saturating_sub(o));
        // An attack long gone (a note re-entered after a slide) is not a reaction.
        let onset = if onset as f32 > 0.25 * self.sample_rate { 0 } else { onset };
        emit(at, VoiceEvent::NoteOn { key, velocity: self.velocity(level_db), onset: onset as u32 });
        if onset > 0 {
            self.reaction = onset as f32 / self.sample_rate;
        }
        self.onset_at = None;
        self.sounding = Some(Sounding { key, start: self.clock, resting: false });
        self.last_bend = 0.0;
        self.candidate = None;
        self.candidate_hops = 0;
    }

    /// The note decision of a hop.
    fn decide(&mut self, at: usize, voiced: bool, pitch: f32, level_db: f32, attack: bool, emit: &mut impl FnMut(usize, VoiceEvent)) {
        let c = self.config;
        let min_age = (c.min_note * self.sample_rate) as u64;
        if !voiced {
            self.stable = 0;
            self.candidate = None;
            self.last_pitch = f32::NAN;
            let Some(s) = self.sounding.filter(|s| !s.resting) else {
                self.quiet_hops = 0;
                return;
            };
            self.quiet_hops += 1;
            let quiet = level_db < c.gate_db - GATE_HYSTERESIS_DB;
            let limit = self.hops(if quiet { RELEASE_QUIET_SECONDS } else { RELEASE_UNVOICED_SECONDS });
            if self.quiet_hops >= limit && self.clock - s.start >= min_age {
                if c.mode == VoiceMode::Hold {
                    self.sounding = Some(Sounding { resting: true, ..s });
                } else {
                    emit(at, VoiceEvent::NoteOff { key: s.key });
                    self.sounding = None;
                    self.period = 0.0;
                }
                self.quiet_hops = 0;
            }
            return;
        }
        self.quiet_hops = 0;
        let agree = (pitch - self.last_pitch).abs() < 0.6;
        self.stable = if agree { self.stable + 1 } else { 1 };
        let p = pitch + f32::from(c.transpose);
        let smoothed = if agree { 0.5 * (pitch + self.last_pitch) + f32::from(c.transpose) } else { p };
        self.last_pitch = pitch;
        let key = snap_to_scale(smoothed, c.scale);
        match self.sounding {
            None => {
                if self.stable >= 2 {
                    self.start(at, key, level_db, emit);
                }
            }
            Some(s) if s.resting => {
                // Hold mode: the voice is back, the next note replaces the held one.
                if self.stable >= 2 {
                    emit(at, VoiceEvent::NoteOff { key: s.key });
                    self.start(at, key, level_db, emit);
                }
            }
            Some(s) => {
                let age = self.clock - s.start;
                let cur = f32::from(s.key);
                if key != s.key && (p - cur).abs() - (p - f32::from(key)).abs() > HYSTERESIS {
                    if self.candidate == Some(key) {
                        self.candidate_hops += 1;
                    } else {
                        self.candidate = Some(key);
                        self.candidate_hops = 1;
                    }
                    let need = if (p - cur).abs() >= LEAP { 2 } else { self.hops(CHANGE_SECONDS) };
                    if self.candidate_hops >= need && age >= min_age {
                        self.onset_at = None;
                        if c.mode == VoiceMode::Legato {
                            self.start(at, key, level_db, emit);
                            emit(at, VoiceEvent::NoteOff { key: s.key });
                        } else {
                            emit(at, VoiceEvent::NoteOff { key: s.key });
                            self.start(at, key, level_db, emit);
                        }
                        return;
                    }
                } else {
                    self.candidate = None;
                    self.candidate_hops = 0;
                }
                if attack && c.mode == VoiceMode::Normal && age >= min_age.max((0.08 * self.sample_rate) as u64) {
                    // Re-articulated on the same pitch.
                    emit(at, VoiceEvent::NoteOff { key: s.key });
                    self.start(at, s.key, level_db, emit);
                    return;
                }
                if c.retune > 0.0 {
                    let bend = ((p - cur) * c.retune).clamp(-12.0, 12.0);
                    if (bend - self.last_bend).abs() > 0.01 {
                        self.last_bend = bend;
                        emit(at, VoiceEvent::Bend { key: s.key, semitones: bend });
                    }
                }
            }
        }
    }
}

/// A note found in a recording: input samples, key and velocity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteSpan {
    pub start: usize,
    pub end: usize,
    pub key: u8,
    pub velocity: f32,
}

/// Offline variant: the notes of a monophonic recording, with the analysis
/// of the real-time tracker. Starts are moved back to the attacks; notes
/// shorter than [`VoiceConfig::min_note`] are dropped. Allocates.
pub fn transcribe(signal: &[f32], sample_rate: f32, config: &VoiceConfig) -> Vec<NoteSpan> {
    let mut t = VoiceTracker::new(sample_rate, VoiceConfig { mode: VoiceMode::Normal, retune: 0.0, ..*config });
    let mut notes: Vec<NoteSpan> = Vec::new();
    let mut open: Option<NoteSpan> = None;
    let tail = vec![0.0; (0.3 * sample_rate) as usize];
    let mut base = 0usize;
    for block in signal.chunks(512).chain(tail.chunks(512)) {
        t.process(block, &mut |at, ev| {
            let pos = base + at;
            match ev {
                VoiceEvent::NoteOn { key, velocity, onset } => {
                    if let Some(mut n) = open.take() {
                        n.end = pos;
                        notes.push(n);
                    }
                    let start = pos.saturating_sub(onset as usize).max(notes.last().map_or(0, |n| n.end));
                    open = Some(NoteSpan { start, end: pos, key, velocity });
                }
                VoiceEvent::NoteOff { key } => {
                    if let Some(mut n) = open.take_if(|n| n.key == key) {
                        // The release is detected after the voice stops.
                        n.end = pos.saturating_sub((RELEASE_QUIET_SECONDS * sample_rate) as usize).max(n.start + 1);
                        notes.push(n);
                    }
                }
                _ => {}
            }
        });
        base += block.len();
    }
    if let Some(mut n) = open {
        n.end = signal.len();
        notes.push(n);
    }
    let shortest = (config.min_note * sample_rate) as usize;
    notes.retain(|n| n.end.min(signal.len()) > n.start && n.end - n.start >= shortest);
    for n in &mut notes {
        n.end = n.end.min(signal.len());
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::WhiteNoise;
    use std::f64::consts::TAU;

    const SR: f32 = 48_000.0;

    /// A sung note: harmonics falling off with a formant bump, vibrato,
    /// jitter, breath noise, soft attack and release.
    #[derive(Clone, Copy)]
    struct Sung {
        /// Pitch at the start and at the end (glide), MIDI.
        from: f32,
        to: f32,
        secs: f32,
        /// Vibrato depth (cents, ±) and rate.
        vibrato: f32,
        rate: f32,
        amp: f32,
        /// Weight of the fundamental (1: normal, 0.2: weak, like some male voices).
        fundamental: f32,
    }

    impl Sung {
        fn note(key: f32, secs: f32) -> Self {
            Self { from: key, to: key, secs, vibrato: 30.0, rate: 5.5, amp: 0.3, fundamental: 1.0 }
        }
    }

    /// Renders notes separated by `gap` seconds of near silence; returns the
    /// signal and the start of each note (samples).
    fn sing(notes: &[Sung], gap: f32, noise_db: f32, seed: u32) -> (Vec<f32>, Vec<usize>) {
        let sr = SR as f64;
        let mut noise = WhiteNoise::new(seed);
        let mut jitter = WhiteNoise::new(seed ^ 0x5eed);
        let noise_gain = 10f32.powf(noise_db / 20.0);
        let mut out = Vec::new();
        let mut starts = Vec::new();
        let silence = |out: &mut Vec<f32>, n: usize, noise: &mut WhiteNoise| {
            for _ in 0..n {
                out.push(noise.next_sample() * noise_gain);
            }
        };
        silence(&mut out, (gap * SR) as usize, &mut noise);
        for s in notes {
            starts.push(out.len());
            let n = (s.secs * SR) as usize;
            let mut phase = 0.0f64;
            let mut drift = 0.0f64;
            for i in 0..n {
                let t = i as f64 / sr;
                let glide = (t / (s.secs as f64).min(0.3)).min(1.0);
                let base = s.from as f64 + (s.to - s.from) as f64 * glide;
                // Vibrato fades in after 150 ms, like a singer's.
                let depth = s.vibrato as f64 / 100.0 * (t / 0.15).min(1.0);
                drift = 0.999 * drift + 0.001 * jitter.next_sample() as f64;
                let midi = base + depth * (TAU * s.rate as f64 * t).sin() + drift * 0.3;
                let hz = 440.0 * 2f64.powf((midi - 69.0) / 12.0);
                phase += hz / sr;
                let mut x = 0.0;
                for k in 1..=24 {
                    let fk = hz * k as f64;
                    if fk > sr * 0.45 {
                        break;
                    }
                    // Glottal tilt, a formant around 700 Hz and one around 2.5 kHz.
                    let tilt = 1.0 / (k as f64).powf(1.1);
                    let f1 = 1.0 + 2.0 / (1.0 + ((fk - 700.0) / 250.0).powi(2));
                    let f2 = 1.0 + 1.0 / (1.0 + ((fk - 2500.0) / 400.0).powi(2));
                    let w = if k == 1 { s.fundamental as f64 } else { 1.0 };
                    x += w * tilt * f1 * f2 * (TAU * phase * k as f64).sin();
                }
                let env = (t / 0.02).min(1.0) * ((s.secs as f64 - t) / 0.03).clamp(0.0, 1.0);
                out.push((x * 0.25 * s.amp as f64 * env) as f32 + noise.next_sample() * noise_gain);
            }
            silence(&mut out, (gap * SR) as usize, &mut noise);
        }
        (out, starts)
    }

    /// Runs the tracker in engine-sized blocks: (sample, event) list.
    fn run(signal: &[f32], config: VoiceConfig) -> Vec<(usize, VoiceEvent)> {
        let mut t = VoiceTracker::new(SR, config);
        let mut events = Vec::new();
        let mut base = 0;
        for block in signal.chunks(256) {
            t.process(block, &mut |at, ev| {
                if !matches!(ev, VoiceEvent::Frame(_)) {
                    events.push((base + at, ev));
                }
            });
            base += block.len();
        }
        events
    }

    fn note_ons(events: &[(usize, VoiceEvent)]) -> Vec<(usize, u8)> {
        events
            .iter()
            .filter_map(|&(at, ev)| match ev {
                VoiceEvent::NoteOn { key, .. } => Some((at, key)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn snapping_to_scales() {
        let major: u16 = [0, 2, 4, 5, 7, 9, 11].iter().map(|&p| 1 << p).sum();
        assert_eq!(snap_to_scale(60.2, 0xFFF), 60);
        assert_eq!(snap_to_scale(60.6, 0), 61);
        assert_eq!(snap_to_scale(61.0, major), 60, "tie: the lower note");
        assert_eq!(snap_to_scale(61.3, major), 62);
        assert_eq!(snap_to_scale(66.0, major), 65);
        assert_eq!(snap_to_scale(66.2, major), 67);
        // A minor pentatonic: A C D E G.
        let penta: u16 = [9, 0, 2, 4, 7].iter().map(|&p| 1 << p).sum();
        assert_eq!(snap_to_scale(58.4, penta), 57);
        assert_eq!(snap_to_scale(59.6, penta), 60);
        assert_eq!(snap_to_scale(65.0, penta), 64);
        assert_eq!(snap_to_scale(-3.0, 0xFFF), 0);
        assert_eq!(snap_to_scale(140.0, 0xFFF), 127);
    }

    #[test]
    fn a_melody_across_the_range_with_measured_latency() {
        let keys = [40.0, 45.0, 48.0, 52.0, 55.0, 57.0, 60.0, 62.0, 64.0, 67.0, 69.0, 72.0, 76.0, 79.0, 84.0];
        let notes: Vec<Sung> = keys.iter().map(|&k| Sung::note(k, 0.45)).collect();
        let (signal, starts) = sing(&notes, 0.18, -60.0, 7);
        let ons = note_ons(&run(&signal, VoiceConfig::default()));
        let got: Vec<u8> = ons.iter().map(|o| o.1).collect();
        let want: Vec<u8> = keys.iter().map(|&k| k as u8).collect();
        assert_eq!(got, want, "one note per sung note, right pitch");
        let lat: Vec<f32> = ons.iter().zip(&starts).map(|(o, &s)| (o.0 - s) as f32 / SR * 1000.0).collect();
        let mean = lat.iter().sum::<f32>() / lat.len() as f32;
        let worst = lat.iter().cloned().fold(0.0, f32::max);
        eprintln!("voice → note latency per note (ms): {lat:.1?}\nmean {mean:.1} ms, worst {worst:.1} ms");
        assert!(mean < 22.0 && worst < 30.0, "mean {mean} ms, worst {worst} ms");
    }

    #[test]
    fn a_noisy_room_keeps_the_notes() {
        // Breath and room noise about 22 dB under the voice.
        let keys = [47.0, 50.0, 55.0, 59.0, 62.0, 66.0, 71.0];
        let notes: Vec<Sung> = keys.iter().map(|&k| Sung { vibrato: 45.0, ..Sung::note(k, 0.5) }).collect();
        let (signal, _) = sing(&notes, 0.2, -42.0, 13);
        let ons = note_ons(&run(&signal, VoiceConfig::default()));
        assert_eq!(ons.iter().map(|o| o.1).collect::<Vec<_>>(), keys.map(|k| k as u8));
    }

    #[test]
    fn higher_range_reacts_faster() {
        let notes: Vec<Sung> = [57.0, 60.0, 64.0, 67.0, 69.0, 72.0].iter().map(|&k| Sung::note(k, 0.4)).collect();
        let (signal, starts) = sing(&notes, 0.2, -60.0, 3);
        let config = VoiceConfig { low: 52, ..VoiceConfig::default() };
        let ons = note_ons(&run(&signal, config));
        assert_eq!(ons.len(), 6);
        let mean = ons.iter().zip(&starts).map(|(o, &s)| (o.0 - s) as f32).sum::<f32>() / 6.0 / SR * 1000.0;
        eprintln!("range from E3: mean latency {mean:.1} ms");
        assert!(mean < 16.0, "{mean} ms");
    }

    #[test]
    fn vibrato_of_fifty_cents_never_flutters() {
        for key in [45.0, 57.0, 64.0, 71.0] {
            let note = Sung { vibrato: 50.0, rate: 6.0, secs: 2.5, ..Sung::note(key, 2.5) };
            let (signal, _) = sing(&[note], 0.2, -55.0, key as u32);
            let events = run(&signal, VoiceConfig::default());
            let ons = note_ons(&events);
            assert_eq!(ons.len(), 1, "key {key}: {ons:?}");
            assert_eq!(ons[0].1, key as u8);
            let offs = events.iter().filter(|e| matches!(e.1, VoiceEvent::NoteOff { .. })).count();
            assert_eq!(offs, 1, "key {key}");
        }
    }

    #[test]
    fn octave_errors_are_rare() {
        // Sixty notes, a third of them with a weak fundamental, some noise.
        let mut notes = Vec::new();
        let mut rng = WhiteNoise::new(99);
        for i in 0..60 {
            let key = (42.0 + (rng.next_sample() * 0.5 + 0.5) * 36.0).round().clamp(42.0, 80.0);
            let fundamental = if i % 3 == 0 { 0.25 } else { 1.0 };
            notes.push(Sung { fundamental, amp: 0.15 + 0.2 * (i % 4) as f32 / 3.0, ..Sung::note(key, 0.35) });
        }
        let (signal, _) = sing(&notes, 0.15, -50.0, 11);
        let ons = note_ons(&run(&signal, VoiceConfig::default()));
        assert_eq!(ons.len(), notes.len(), "one note each");
        let errors = ons.iter().zip(&notes).filter(|(o, n)| o.1 != n.from as u8).count();
        let octave = ons.iter().zip(&notes).filter(|(o, n)| (o.1 as i32 - n.from as i32).abs() % 12 == 0 && o.1 != n.from as u8).count();
        eprintln!("60 notes: {errors} wrong, {octave} octave errors");
        assert!(octave <= 1 && errors <= 1, "{errors} wrong, {octave} octave errors");
    }

    #[test]
    fn a_glide_steps_up_without_going_back() {
        let glide = Sung { from: 57.0, to: 64.0, secs: 0.9, vibrato: 0.0, ..Sung::note(57.0, 0.9) };
        let (signal, _) = sing(&[glide], 0.2, -60.0, 5);
        let ons = note_ons(&run(&signal, VoiceConfig::default()));
        let keys: Vec<u8> = ons.iter().map(|o| o.1).collect();
        assert_eq!(keys.first(), Some(&57));
        assert_eq!(keys.last(), Some(&64));
        assert!(keys.windows(2).all(|w| w[1] > w[0]), "steps only upward: {keys:?}");
    }

    #[test]
    fn scale_transpose_and_range() {
        // C#4 sung a little high snaps to D in C major; transposed by an octave.
        let major: u16 = [0, 2, 4, 5, 7, 9, 11].iter().map(|&p| 1 << p).sum();
        let note = Sung { vibrato: 10.0, ..Sung::note(61.3, 0.5) };
        let (signal, _) = sing(&[note], 0.2, -60.0, 1);
        let ons = note_ons(&run(&signal, VoiceConfig { scale: major, ..VoiceConfig::default() }));
        assert_eq!(ons.iter().map(|o| o.1).collect::<Vec<_>>(), [62]);
        let ons = note_ons(&run(&signal, VoiceConfig { scale: major, transpose: 12, ..VoiceConfig::default() }));
        assert_eq!(ons.iter().map(|o| o.1).collect::<Vec<_>>(), [74]);
        let ons = note_ons(&run(&signal, VoiceConfig { transpose: -7, ..VoiceConfig::default() }));
        assert_eq!(ons.iter().map(|o| o.1).collect::<Vec<_>>(), [54]);
        // Out of the voice's range: ignored.
        let ons = note_ons(&run(&signal, VoiceConfig { low: 64, high: 84, ..VoiceConfig::default() }));
        assert!(ons.is_empty(), "{ons:?}");
    }

    #[test]
    fn silence_and_noise_play_nothing() {
        let quiet = vec![0.0; (SR * 1.0) as usize];
        assert!(run(&quiet, VoiceConfig::default()).is_empty());
        let mut noise = WhiteNoise::new(4);
        let loud: Vec<f32> = (0..(SR * 2.0) as usize).map(|_| noise.next_sample() * 0.3).collect();
        let ons = note_ons(&run(&loud, VoiceConfig::default()));
        assert!(ons.is_empty(), "noise played {ons:?}");
    }

    #[test]
    fn retune_follows_the_bends_or_snaps_hard() {
        let note = Sung { vibrato: 40.0, ..Sung::note(60.0, 1.0) };
        let (signal, _) = sing(&[note], 0.2, -60.0, 2);
        let hard = run(&signal, VoiceConfig::default());
        assert!(!hard.iter().any(|e| matches!(e.1, VoiceEvent::Bend { .. })));
        let soft = run(&signal, VoiceConfig { retune: 1.0, ..VoiceConfig::default() });
        let bends: Vec<f32> = soft
            .iter()
            .filter_map(|e| match e.1 {
                VoiceEvent::Bend { semitones, key: 60 } => Some(semitones),
                _ => None,
            })
            .collect();
        let (lo, hi) = bends.iter().fold((0.0f32, 0.0f32), |(a, b), &x| (a.min(x), b.max(x)));
        assert!(bends.len() > 50 && lo < -0.25 && hi > 0.25 && lo > -0.6 && hi < 0.6, "{} bends in {lo}..{hi}", bends.len());
    }

    #[test]
    fn repeated_attacks_retrigger_in_normal_mode_only() {
        // Three "da" on one pitch, the level dipping between them.
        let sr = SR as usize;
        let mut signal = Vec::new();
        for _ in 0..3 {
            let (s, _) = sing(&[Sung::note(57.0, 0.3)], 0.0, -70.0, 8);
            signal.extend(s.iter().map(|x| x * 1.0));
            // A dip of 25 dB, still pitched.
            signal.extend(s[sr / 10..sr / 10 + sr / 25].iter().map(|x| x * 0.05));
        }
        signal.extend(std::iter::repeat_n(0.0, sr / 4));
        let normal = note_ons(&run(&signal, VoiceConfig::default()));
        assert_eq!(normal.len(), 3, "{normal:?}");
        let legato = note_ons(&run(&signal, VoiceConfig { mode: VoiceMode::Legato, ..VoiceConfig::default() }));
        assert!(legato.len() <= 3);
    }

    #[test]
    fn hold_mode_sustains_through_silences() {
        let (signal, _) = sing(&[Sung::note(57.0, 0.3), Sung::note(62.0, 0.3)], 0.3, -70.0, 6);
        let events = run(&signal, VoiceConfig { mode: VoiceMode::Hold, ..VoiceConfig::default() });
        let kinds: Vec<String> = events
            .iter()
            .map(|e| match e.1 {
                VoiceEvent::NoteOn { key, .. } => format!("on {key}"),
                VoiceEvent::NoteOff { key } => format!("off {key}"),
                _ => String::new(),
            })
            .collect();
        assert_eq!(kinds, ["on 57", "off 57", "on 62"], "the last note keeps sounding");
        // The first note ends when the second starts, not at the silence.
        assert_eq!(events[1].0, events[2].0);
    }

    #[test]
    fn legato_overlaps_the_next_note() {
        let glide = Sung { from: 60.0, to: 67.0, secs: 0.6, vibrato: 0.0, ..Sung::note(60.0, 0.6) };
        let (signal, _) = sing(&[glide], 0.2, -60.0, 5);
        let events = run(&signal, VoiceConfig { mode: VoiceMode::Legato, ..VoiceConfig::default() });
        let first_off = events.iter().position(|e| matches!(e.1, VoiceEvent::NoteOff { .. })).unwrap();
        assert!(matches!(events[first_off - 1].1, VoiceEvent::NoteOn { .. }), "the next note starts before the previous ends");
    }

    #[test]
    fn offline_transcription_places_notes_on_the_attacks() {
        let keys = [52.0, 55.0, 59.0, 64.0, 62.0, 59.0];
        let notes: Vec<Sung> = keys.iter().map(|&k| Sung::note(k, 0.35)).collect();
        let (signal, starts) = sing(&notes, 0.12, -60.0, 21);
        let spans = transcribe(&signal, SR, &VoiceConfig::default());
        assert_eq!(spans.iter().map(|n| n.key).collect::<Vec<_>>(), keys.map(|k| k as u8));
        for (n, &s) in spans.iter().zip(&starts) {
            let early = (n.start as f32 - s as f32) / SR * 1000.0;
            assert!(early.abs() < 12.0, "start off by {early} ms");
            let len = (n.end - n.start) as f32 / SR;
            assert!((len - 0.35).abs() < 0.06, "length {len} s");
            assert!(n.velocity > 0.2 && n.velocity <= 1.0);
        }
        // Too short to keep.
        let blip = sing(&[Sung::note(60.0, 0.03)], 0.2, -70.0, 2).0;
        assert!(transcribe(&blip, SR, &VoiceConfig { min_note: 0.08, ..VoiceConfig::default() }).is_empty());
    }

    #[test]
    fn cpu_cost_is_a_small_share_of_real_time() {
        let notes: Vec<Sung> = (0..20).map(|i| Sung::note(40.0 + (i * 5 % 36) as f32, 0.45)).collect();
        let (signal, _) = sing(&notes, 0.05, -50.0, 17);
        let config = VoiceConfig { low: LOWEST, retune: 1.0, ..VoiceConfig::default() };
        let mut t = VoiceTracker::new(SR, config);
        let started = std::time::Instant::now();
        let mut count = 0usize;
        for block in signal.chunks(128) {
            t.process(block, &mut |_, _| count += 1);
        }
        let share = started.elapsed().as_secs_f32() / (signal.len() as f32 / SR);
        eprintln!("voice tracker, widest range: {:.2} % of one core in real time", share * 100.0);
        assert!(count > 0 && share < 0.2, "{share}");
    }

    #[test]
    fn other_sample_rates() {
        for sr in [44_100.0f32, 96_000.0] {
            let n = (sr * 0.5) as usize;
            let x: Vec<f32> = (0..n)
                .map(|i| (1..=8).map(|k| (TAU * 220.0 * k as f64 * i as f64 / sr as f64).sin() / k as f64).sum::<f64>() as f32 * 0.2)
                .chain(std::iter::repeat_n(0.0, n / 2))
                .collect();
            let mut t = VoiceTracker::new(sr, VoiceConfig::default());
            let mut keys = Vec::new();
            t.process(&x, &mut |_, ev| {
                if let VoiceEvent::NoteOn { key, .. } = ev {
                    keys.push(key);
                }
            });
            assert_eq!(keys, [57], "at {sr} Hz");
        }
    }
}
