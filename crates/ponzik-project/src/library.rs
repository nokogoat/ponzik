//! Bundled sound library, rendered by Ponzik itself: one-shots, 808s, synth
//! sounds, FX and drum loops, built from the native instruments and effects
//! (plus a few noise and oscillator layers for cymbals, shakers and risers).
//!
//! Everything is deterministic: the same build always writes the same bytes,
//! so the library never ships in the repository and carries no licensing risk.
//! Files are 24-bit / 48 kHz stereo WAV, one-shots peak at −1 dBFS, melodic
//! sounds carry their note in the name, loops their tempo.

use std::f32::consts::{PI, TAU};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ponzik_dsp::filter::{DcBlocker, FilterMode, Svf};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::midi_to_hz;
use ponzik_engine::registry;

use crate::render::{BitDepth, encode_wav};

/// Sample rate of every library file.
pub const LIBRARY_SAMPLE_RATE: u32 = 48_000;
/// Bumped whenever a sound changes: the app rebuilds an older library.
pub const LIBRARY_VERSION: u32 = 1;
/// Marker written last, once every file is in place.
pub const LIBRARY_MARKER: &str = ".ponzik-library";

const SR: f32 = LIBRARY_SAMPLE_RATE as f32;
const BLOCK: usize = 128;
/// Peak ceiling of every file: −1 dBFS.
const CEILING: f32 = 0.891_250_9;

// --- Buffers -------------------------------------------------------------------

/// A stereo buffer being built.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Buf {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

fn frames(secs: f32) -> usize {
    (secs.max(0.0) * SR).round() as usize
}

fn db(v: f32) -> f32 {
    10f32.powf(v / 20.0)
}

impl Buf {
    fn new(len: usize) -> Self {
        Self { l: vec![0.0; len], r: vec![0.0; len] }
    }

    fn len(&self) -> usize {
        self.l.len()
    }

    /// Adds `other` at `at` frames, growing the buffer if needed.
    fn mix_at(&mut self, other: &Buf, at: usize, gain: f32) {
        let end = at + other.len();
        if end > self.len() {
            self.l.resize(end, 0.0);
            self.r.resize(end, 0.0);
        }
        for i in 0..other.len() {
            self.l[at + i] += other.l[i] * gain;
            self.r[at + i] += other.r[i] * gain;
        }
    }

    fn mix(mut self, other: &Buf, at_secs: f32, gain: f32) -> Self {
        self.mix_at(other, frames(at_secs), gain);
        self
    }

    fn gain(mut self, g: f32) -> Self {
        self.l.iter_mut().chain(self.r.iter_mut()).for_each(|s| *s *= g);
        self
    }

    /// Constant-power pan in [−1, 1].
    fn pan(mut self, p: f32) -> Self {
        let a = (p.clamp(-1.0, 1.0) + 1.0) * PI / 4.0;
        let (gl, gr) = (a.cos() * 2f32.sqrt(), a.sin() * 2f32.sqrt());
        self.l.iter_mut().for_each(|s| *s *= gl);
        self.r.iter_mut().for_each(|s| *s *= gr);
        self
    }

    /// Appends silence so effects can ring out.
    fn tail(mut self, secs: f32) -> Self {
        let n = self.len() + frames(secs);
        self.l.resize(n, 0.0);
        self.r.resize(n, 0.0);
        self
    }

    fn reverse(mut self) -> Self {
        self.l.reverse();
        self.r.reverse();
        self
    }

    fn peak(&self) -> f32 {
        self.l.iter().chain(&self.r).fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Loudest 100 ms window, RMS of both channels.
    fn rms_max(&self) -> f32 {
        let w = frames(0.1).max(1);
        let mut best = 0.0f32;
        let mut start = 0;
        while start < self.len() {
            let end = (start + w).min(self.len());
            let sum: f32 = (start..end).map(|i| self.l[i] * self.l[i] + self.r[i] * self.r[i]).sum();
            best = best.max((sum / (2 * w) as f32).sqrt());
            start += w / 2;
        }
        best
    }

    fn fade_in(mut self, secs: f32) -> Self {
        let n = frames(secs).min(self.len());
        for i in 0..n {
            let g = 0.5 - 0.5 * (PI * i as f32 / n as f32).cos();
            self.l[i] *= g;
            self.r[i] *= g;
        }
        self
    }

    fn fade_out(mut self, secs: f32) -> Self {
        let len = self.len();
        let n = frames(secs).min(len);
        for i in 0..n {
            let g = 0.5 - 0.5 * (PI * i as f32 / n as f32).cos();
            self.l[len - 1 - i] *= g;
            self.r[len - 1 - i] *= g;
        }
        self
    }

    /// Cuts the tail once it stays 66 dB under the peak (plus a short margin).
    fn trim(mut self) -> Self {
        let floor = self.peak() * db(-66.0);
        let last = (0..self.len()).rev().find(|&i| self.l[i].abs() > floor || self.r[i].abs() > floor).unwrap_or(0);
        let end = (last + frames(0.03)).min(self.len());
        self.l.truncate(end);
        self.r.truncate(end);
        self
    }

    fn dc_block(mut self) -> Self {
        let (mut a, mut b) = (DcBlocker::new(SR), DcBlocker::new(SR));
        self.l.iter_mut().for_each(|s| *s = a.process(*s));
        self.r.iter_mut().for_each(|s| *s = b.process(*s));
        self
    }

    /// Interleaved samples.
    pub fn interleaved(&self) -> Vec<f32> {
        self.l.iter().zip(&self.r).flat_map(|(&l, &r)| [l, r]).collect()
    }
}

/// How a finished sound is levelled.
#[derive(Clone, Copy)]
enum Level {
    /// Peak at −1 dBFS.
    Peak,
    /// Loudest 100 ms at this RMS (dBFS), never over −1 dBFS peak.
    Rms(f32),
    /// Like `Rms`, with a limiter catching the few peaks over −1 dBFS
    /// (chords whose oscillators start in phase) instead of turning it all down.
    Loud(f32),
}

fn level(buf: Buf, level: Level) -> Buf {
    let peak = buf.peak();
    if peak <= 1e-9 {
        return buf;
    }
    match level {
        Level::Peak => buf.gain(CEILING / peak),
        Level::Rms(target) => {
            let g = (db(target) / buf.rms_max().max(1e-9)).min(CEILING / peak);
            buf.gain(g)
        }
        Level::Loud(target) => {
            let g = db(target) / buf.rms_max().max(1e-9);
            let b = buf.gain(g);
            let b = if b.peak() > CEILING { fx(b, "limiter", &[(1, -1.3), (2, 0.05)]) } else { b };
            let peak = b.peak();
            if peak > CEILING { b.gain(CEILING / peak) } else { b }
        }
    }
}

/// Finishes a one-shot: DC removed, tail trimmed, declicked at both ends, levelled.
fn one_shot(buf: Buf, lvl: Level) -> Buf {
    let buf = buf.dc_block().trim().fade_in(0.000_3).fade_out(0.025);
    level(buf, lvl)
}

/// Like [`one_shot`] with a gentler attack, for pads and risers.
fn soft_shot(buf: Buf, lvl: Level) -> Buf {
    let buf = buf.dc_block().trim().fade_in(0.002).fade_out(0.08);
    level(buf, lvl)
}

// --- Native instruments and effects ------------------------------------------

type P<'a> = &'a [(u32, f32)];

/// A note played on an instrument: start and length in seconds.
#[derive(Clone, Copy)]
struct N {
    t: f32,
    note: u8,
    vel: f32,
    len: f32,
}

const fn n(t: f32, note: u8, vel: f32, len: f32) -> N {
    N { t, note, vel, len }
}

/// Plays `notes` on the native instrument `kind` and renders `secs` seconds.
fn play(kind: &str, params: P, notes: &[N], secs: f32) -> Buf {
    let mut inst = registry::create_instrument(kind).unwrap_or_else(|| panic!("instrument {kind}"));
    inst.set_sample_rate(SR);
    for &(id, v) in params {
        let info = inst.param_info(id).unwrap_or_else(|| panic!("{kind}: no param {id}"));
        inst.set_param(id, info.clamp(v));
    }
    let total = frames(secs);
    let mut events: Vec<(usize, bool, u8, f32)> = Vec::new();
    for x in notes {
        events.push((frames(x.t), true, x.note, x.vel));
        events.push((frames(x.t + x.len), false, x.note, 0.0));
    }
    // Releases before attacks on the same frame.
    events.sort_by_key(|e| (e.0, e.1));
    let mut out = Buf::new(total);
    let (mut pos, mut k) = (0, 0);
    while pos < total {
        while k < events.len() && events[k].0 <= pos {
            let (_, on, note, vel) = events[k];
            if on { inst.note_on(note, vel) } else { inst.note_off(note) }
            k += 1;
        }
        let next = events.get(k).map_or(total, |e| e.0);
        let end = (pos + BLOCK).min(next).min(total);
        let (l, r) = (&mut out.l[pos..end], &mut out.r[pos..end]);
        inst.render(l, r);
        pos = end;
    }
    out
}

/// One note, held `len` seconds, rendered for `secs`.
fn hit(kind: &str, params: P, note: u8, len: f32, secs: f32) -> Buf {
    play(kind, params, &[n(0.0, note, 1.0, len)], secs)
}

/// A chord: every note together.
fn chord(kind: &str, params: P, notes: &[u8], len: f32, secs: f32) -> Buf {
    let ns: Vec<N> = notes.iter().map(|&k| n(0.0, k, 0.85, len)).collect();
    play(kind, params, &ns, secs)
}

/// Runs the native effect `kind` over the buffer (latency compensated).
fn fx_at(mut buf: Buf, kind: &str, params: P, bpm: f32) -> Buf {
    let mut e = registry::create_effect(kind).unwrap_or_else(|| panic!("effect {kind}"));
    e.set_sample_rate(SR);
    e.set_tempo(bpm);
    for &(id, v) in params {
        let info = e.param_info(id).unwrap_or_else(|| panic!("{kind}: no param {id}"));
        e.set_param(id, info.clamp(v));
    }
    let lat = e.latency();
    let len = buf.len();
    buf.l.resize(len + lat, 0.0);
    buf.r.resize(len + lat, 0.0);
    let mut pos = 0;
    while pos < buf.len() {
        let end = (pos + BLOCK).min(buf.len());
        e.process(&mut buf.l[pos..end], &mut buf.r[pos..end]);
        pos = end;
    }
    buf.l.drain(..lat);
    buf.r.drain(..lat);
    buf
}

fn fx(buf: Buf, kind: &str, params: P) -> Buf {
    fx_at(buf, kind, params, 120.0)
}

// Effect parameter shortcuts.
fn eq(buf: Buf, params: P) -> Buf {
    fx(buf, "eq", params)
}
fn sat(buf: Buf, kind: f32, drive: f32, tone: f32) -> Buf {
    fx(buf, "saturator", &[(0, kind), (1, drive), (2, tone)])
}
fn verb(buf: Buf, mode: f32, size: f32, decay: f32, damping_hz: f32, mix: f32) -> Buf {
    let tail = decay.min(6.0) + 0.3;
    fx(buf.tail(tail), "vintage_verb", &[(0, mode), (1, 0.01), (2, size), (4, decay), (5, damping_hz), (6, 150.0), (11, mix)])
}
fn room(buf: Buf, mix: f32) -> Buf {
    fx(buf.tail(0.8), "reverb", &[(0, 0.004), (1, 0.25), (2, 0.5), (3, 0.7), (5, mix)])
}
fn crush(buf: Buf, bits: f32, down: f32, mix: f32) -> Buf {
    fx(buf, "bitcrusher", &[(0, bits), (1, down), (2, mix)])
}
fn punch(buf: Buf, attack: f32, sustain: f32) -> Buf {
    fx(buf, "transient", &[(0, attack), (1, sustain)])
}
fn comp(buf: Buf, threshold: f32, ratio: f32, attack: f32, release: f32, makeup: f32) -> Buf {
    fx(buf, "compressor", &[(0, threshold), (1, ratio), (4, attack), (5, release), (6, makeup)])
}

// --- Hand-built layers (noise, metal, tones) -----------------------------------

/// Renders `secs` seconds from a per-sample closure (time in seconds → stereo).
fn synth(secs: f32, mut f: impl FnMut(f32) -> (f32, f32)) -> Buf {
    let len = frames(secs);
    let mut b = Buf::new(len);
    for i in 0..len {
        let (l, r) = f(i as f32 / SR);
        b.l[i] = l;
        b.r[i] = r;
    }
    b
}

/// Attack then exponential decay.
fn env(t: f32, attack: f32, tau: f32) -> f32 {
    if t < attack { t / attack } else { (-(t - attack) / tau).exp() }
}

/// Filtered noise burst, decorrelated left and right by `width`.
#[allow(clippy::too_many_arguments)]
fn noise(secs: f32, seed: u32, mode: FilterMode, hz: f32, q: f32, attack: f32, tau: f32, width: f32) -> Buf {
    let (mut a, mut b) = (WhiteNoise::new(seed), WhiteNoise::new(seed.wrapping_mul(7919).wrapping_add(17)));
    let (mut fa, mut fb) = (Svf::new(mode, hz, q, SR), Svf::new(mode, hz, q, SR));
    synth(secs, |t| {
        let e = env(t, attack, tau);
        let (x, y) = (a.next_sample(), b.next_sample());
        let (sa, sb) = (fa.process(x), fb.process(x * (1.0 - width) + y * width));
        (sa * e, sb * e)
    })
}

/// Sine whose pitch falls from `f0` to `f1` (time constant `ptau`), decaying with `tau`.
fn tone(secs: f32, f0: f32, f1: f32, ptau: f32, tau: f32) -> Buf {
    let mut phase = 0.0f32;
    synth(secs, |t| {
        let f = f1 + (f0 - f1) * (-t / ptau).exp();
        phase = (phase + f / SR).fract();
        let s = (TAU * phase).sin() * env(t, 0.000_5, tau);
        (s, s)
    })
}

/// Bank of square oscillators at inharmonic ratios: the metal of hats and cymbals.
fn metal(secs: f32, freqs: &[f32], hp: f32, attack: f32, tau: f32) -> Buf {
    let mut phases = vec![0.0f32; freqs.len()];
    let (mut fl, mut fr) = (Svf::new(FilterMode::HighPass, hp, 0.8, SR), Svf::new(FilterMode::HighPass, hp * 1.07, 0.8, SR));
    synth(secs, |t| {
        let (mut l, mut r) = (0.0, 0.0);
        for (k, (p, f)) in phases.iter_mut().zip(freqs).enumerate() {
            *p = (*p + f / SR).fract();
            let s = if *p < 0.5 { 1.0 } else { -1.0 };
            if k % 2 == 0 { l += s; r += s * 0.7 } else { r += s; l += s * 0.7 }
        }
        let e = env(t, attack, tau) / freqs.len() as f32;
        (fl.process(l) * e, fr.process(r) * e)
    })
}

const METAL_808: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

fn scaled(freqs: &[f32], k: f32) -> Vec<f32> {
    freqs.iter().map(|f| f * k).collect()
}

/// Inharmonic partials for cymbals, deterministic.
fn cymbal_partials(seed: u32, count: usize, lo: f32, hi: f32) -> Vec<f32> {
    let mut rng = WhiteNoise::new(seed);
    (0..count).map(|_| lo * (hi / lo).powf(rng.next_sample() * 0.5 + 0.5)).collect()
}

// --- Drums -----------------------------------------------------------------------

/// Kick instrument: the note sets the pitch (note 60 → A 55 Hz).
fn kick(params: P, secs: f32) -> Buf {
    hit("kick", params, 60, 0.1, secs)
}

fn kick_house(v: u32) -> Buf {
    let (tune, decay, click) = [(0.0, 0.38, 0.55), (-2.0, 0.45, 0.45), (1.0, 0.32, 0.65)][v as usize % 3];
    let k = kick(&[(0, tune), (1, 30.0), (2, 0.035), (3, 0.6), (4, decay), (5, click), (6, 4_500.0), (7, 0.35), (8, 0.35)], 0.9);
    let k = punch(k, 0.35, -0.1);
    let k = eq(k, &[(0, 1.0), (1, 25.0), (3, 60.0), (4, 2.0), (5, 350.0), (6, -3.0), (7, 1.2), (14, 6_000.0), (15, 1.5)]);
    one_shot(sat(k, 0.0, 4.0, 0.0), Level::Peak)
}

fn kick_808(v: u32) -> Buf {
    let (tune, decay) = [(-2.0, 2.2), (0.0, 1.6), (2.0, 2.8)][v as usize % 3];
    let k = kick(&[(0, tune), (1, 24.0), (2, 0.05), (4, decay), (5, 0.15), (7, 0.6), (8, 0.25), (9, 5_000.0)], 3.5);
    let k = sat(k, 0.0, 3.0, 0.0);
    one_shot(eq(k, &[(0, 1.0), (1, 22.0)]), Level::Peak)
}

fn kick_hardstyle(v: u32) -> Buf {
    let (tune, drive) = [(0.0, 22.0), (-1.0, 26.0)][v as usize % 2];
    let k = kick(&[(0, tune), (1, 42.0), (2, 0.03), (3, 0.7), (4, 0.85), (5, 0.6), (8, 1.0), (9, 12_000.0)], 1.2);
    let k = sat(k, 1.0, drive, 2.0).gain(0.3);
    let k = fx(k, "char_saturation", &[(0, 2.0), (1, 12.0), (9, -8.0)]);
    let k = eq(k, &[(0, 1.0), (1, 30.0), (5, 180.0), (6, 4.0), (7, 1.5), (8, 2_500.0), (9, -3.0), (10, 1.0), (16, 1.0), (17, 10_000.0)]);
    one_shot(punch(k, 0.5, 0.0), Level::Peak)
}

fn kick_lofi(v: u32) -> Buf {
    let tune = [0.0, -3.0][v as usize % 2];
    let k = kick(&[(0, tune), (1, 28.0), (2, 0.04), (4, 0.45), (5, 0.25), (6, 2_500.0), (8, 0.4), (9, 3_500.0)], 0.9);
    let k = crush(k, 10.0, 2.5, 0.7);
    let k = eq(k, &[(0, 1.0), (1, 35.0), (16, 1.0), (17, 5_000.0)]);
    one_shot(sat(k, 2.0, 8.0, -2.0), Level::Peak)
}

fn kick_techno(v: u32) -> Buf {
    let tune = [-1.0, -4.0][v as usize % 2];
    let k = kick(&[(0, tune), (1, 34.0), (2, 0.045), (4, 0.55), (5, 0.4), (8, 0.55)], 1.0);
    let k = room(k, 0.12);
    one_shot(eq(k, &[(0, 1.0), (1, 30.0), (5, 120.0), (6, 2.0)]), Level::Peak)
}

/// Drum machine pads: 36 kick, 37 rim, 38 snare, 39 clap, 40 open hat, 41 low tom, 42 closed hat, 43 high tom.
fn pad(pad: u32, tune: f32, decay: f32, secs: f32) -> Buf {
    let base = pad * 3;
    hit("drum_machine", &[(base, tune), (base + 1, decay), (24, 0.1)], 36 + pad as u8, 0.05, secs)
}

/// DrumSynth: 0 kick, 1 snare, 2 clap, 3 closed hat, 4 open hat.
fn drumsynth(kind: f32, tune: f32, decay: f32, tone: f32, secs: f32) -> Buf {
    hit("drumsynth", &[(0, kind), (1, tune), (2, decay), (3, tone)], 60, 0.05, secs)
}

fn snare(v: u32) -> Buf {
    let b = match v {
        // Tight.
        0 => {
            let s = pad(2, 2.0, 0.7, 0.6)
                .mix(&noise(0.4, 11, FilterMode::HighPass, 3_000.0, 0.7, 0.0005, 0.07, 0.3), 0.0, 0.25)
                .mix(&tone(0.3, 280.0, 200.0, 0.01, 0.06), 0.0, 0.5);
            eq(punch(s, 0.4, -0.2), &[(0, 1.0), (1, 120.0), (5, 220.0), (6, 2.0), (14, 7_000.0), (15, 1.0)])
        }
        // Fat.
        1 => {
            let s = pad(2, -3.0, 1.4, 0.8)
                .mix(&tone(0.5, 260.0, 180.0, 0.01, 0.11), 0.0, 0.6)
                .mix(&noise(0.6, 12, FilterMode::BandPass, 5_000.0, 0.6, 0.001, 0.16, 0.4), 0.0, 0.5);
            comp(sat(s, 2.0, 6.0, 0.0), -16.0, 4.0, 0.006, 0.12, 4.0)
        }
        // Trap.
        2 => {
            let s = pad(2, 4.0, 0.8, 0.6).mix(&pad(3, 2.0, 0.8, 0.6), 0.004, 0.6);
            verb(eq(s, &[(0, 1.0), (1, 180.0), (14, 8_000.0), (15, 3.0)]), 1.0, 0.3, 0.8, 9_000.0, 0.14)
        }
        // Lofi.
        3 => {
            let s = pad(2, -1.0, 1.1, 0.7).mix(&noise(0.5, 13, FilterMode::BandPass, 3_500.0, 0.8, 0.001, 0.12, 0.2), 0.0, 0.4);
            let s = crush(sat(s, 2.0, 9.0, -2.0), 9.0, 3.0, 0.8);
            eq(s, &[(0, 1.0), (1, 140.0), (16, 1.0), (17, 6_000.0)])
        }
        // Breakbeat.
        4 => {
            let s = drumsynth(1.0, 1.0, 1.2, 0.6, 0.7).mix(&noise(0.5, 14, FilterMode::BandPass, 2_500.0, 0.9, 0.001, 0.1, 0.3), 0.0, 0.5);
            comp(sat(s, 1.0, 10.0, 1.0).gain(0.5), -20.0, 6.0, 0.003, 0.08, 6.0)
        }
        // Roomy.
        5 => {
            let s = pad(2, 1.0, 0.9, 0.6).mix(&tone(0.4, 240.0, 190.0, 0.01, 0.08), 0.0, 0.4);
            verb(s, 0.0, 0.35, 1.1, 7_000.0, 0.28)
        }
        // DnB: crisp and high.
        6 => {
            let s = pad(2, 5.0, 0.75, 0.5)
                .mix(&noise(0.3, 15, FilterMode::HighPass, 4_500.0, 0.7, 0.0005, 0.06, 0.4), 0.0, 0.35)
                .mix(&tone(0.3, 320.0, 240.0, 0.008, 0.05), 0.0, 0.5);
            eq(punch(s, 0.5, -0.1), &[(0, 1.0), (1, 150.0), (8, 5_000.0), (9, 3.0), (10, 1.0)])
        }
        // Snappy 909.
        _ => {
            let s = drumsynth(1.0, 2.0, 1.0, 0.85, 0.6).mix(&pad(2, 0.0, 0.8, 0.6), 0.0, 0.5);
            punch(s, 0.3, 0.1)
        }
    };
    one_shot(b, Level::Peak)
}

fn clap(v: u32) -> Buf {
    let b = match v {
        0 => eq(punch(pad(3, 0.0, 0.8, 0.6), 0.3, -0.2), &[(0, 1.0), (1, 300.0)]),
        1 => verb(pad(3, -1.0, 1.0, 0.6), 1.0, 0.5, 1.3, 8_000.0, 0.3),
        2 => {
            let a = pad(3, 2.0, 0.9, 0.6).pan(-0.3);
            let b = drumsynth(2.0, 3.0, 1.0, 0.6, 0.6).pan(0.3);
            a.mix(&b, 0.008, 0.8).mix(&pad(2, 3.0, 0.6, 0.4), 0.0, 0.25)
        }
        3 => {
            let a = pad(3, 0.0, 0.9, 0.6).pan(-0.7);
            let b = pad(3, 1.0, 0.9, 0.6).pan(0.7);
            fx(a.mix(&b, 0.015, 0.9), "chorus", &[(2, 0.3), (5, 0.3)])
        }
        4 => eq(crush(pad(3, -2.0, 1.0, 0.6), 8.0, 3.0, 0.7), &[(0, 1.0), (1, 250.0), (16, 1.0), (17, 6_500.0)]),
        _ => room(drumsynth(2.0, 0.0, 1.2, 0.6, 0.6), 0.2),
    };
    one_shot(b, Level::Peak)
}

fn rim(v: u32) -> Buf {
    let b = match v {
        0 => pad(1, 0.0, 1.0, 0.3),
        1 => room(pad(1, 3.0, 1.0, 0.3), 0.25),
        2 => eq(pad(1, -2.0, 1.4, 0.3), &[(5, 900.0), (6, 3.0), (7, 1.5)]),
        _ => crush(pad(1, 1.0, 1.0, 0.3), 8.0, 2.0, 0.8),
    };
    one_shot(b, Level::Peak)
}

fn hat_closed(v: u32) -> Buf {
    let b = match v {
        0 => pad(6, 0.0, 0.6, 0.3),
        1 => eq(drumsynth(3.0, 2.0, 1.0, 0.9, 0.3), &[(14, 10_000.0), (15, 3.0)]),
        2 => {
            let m = metal(0.3, &scaled(&METAL_808, 1.4), 6_000.0, 0.0005, 0.03);
            eq(m.mix(&noise(0.3, 21, FilterMode::HighPass, 7_000.0, 0.7, 0.0005, 0.02, 0.5), 0.0, 0.3), &[(16, 1.0), (17, 9_000.0)])
        }
        3 => pad(6, 3.0, 0.4, 0.2),
        4 => eq(crush(pad(6, -1.0, 0.7, 0.3), 8.0, 2.0, 0.7), &[(16, 1.0), (17, 8_000.0)]),
        _ => noise(0.3, 22, FilterMode::HighPass, 8_000.0, 0.6, 0.004, 0.035, 0.6),
    };
    one_shot(b, Level::Peak)
}

fn hat_open(v: u32) -> Buf {
    let b = match v {
        0 => pad(4, 0.0, 1.0, 1.0),
        1 => pad(4, -1.0, 2.0, 1.6),
        2 => sat(drumsynth(4.0, 0.0, 1.2, 0.8, 1.0), 1.0, 10.0, 2.0),
        3 => eq(pad(4, -3.0, 1.4, 1.2), &[(16, 1.0), (17, 7_000.0)]),
        _ => {
            let m = metal(1.3, &scaled(&METAL_808, 1.7), 7_000.0, 0.001, 0.3);
            m.mix(&noise(1.3, 23, FilterMode::HighPass, 9_000.0, 0.7, 0.001, 0.25, 0.7), 0.0, 0.4)
        }
    };
    one_shot(b, Level::Peak)
}

fn shaker(v: u32) -> Buf {
    let grain = |seed: u32, hz: f32, attack: f32, tau: f32| noise(0.4, seed, FilterMode::BandPass, hz, 1.2, attack, tau, 0.5);
    let b = match v {
        0 => grain(31, 6_500.0, 0.012, 0.05),
        1 => eq(grain(32, 9_000.0, 0.006, 0.04), &[(14, 9_000.0), (15, 3.0)]),
        2 => grain(33, 7_000.0, 0.01, 0.035).mix(&grain(34, 7_500.0, 0.008, 0.05), 0.055, 0.9),
        _ => grain(35, 6_000.0, 0.04, 0.12),
    };
    one_shot(b, Level::Peak)
}

fn perc(v: u32) -> Buf {
    let conga = |f: f32, seed: u32| {
        tone(0.6, f * 1.5, f, 0.008, 0.18).mix(&noise(0.1, seed, FilterMode::BandPass, 2_500.0, 1.5, 0.0005, 0.008, 0.0), 0.0, 0.3)
    };
    let b = match v {
        0 => conga(330.0, 41),
        1 => conga(220.0, 42),
        2 => tone(0.4, 600.0, 400.0, 0.006, 0.09),
        // Cowbell: two squares through a band-pass, as on the 808.
        3 => {
            let m = metal(0.8, &[587.0, 845.0], 300.0, 0.0005, 0.12).mix(&metal(0.8, &[587.0, 845.0], 300.0, 0.0005, 0.03), 0.0, 1.5);
            eq(m, &[(0, 1.0), (1, 400.0), (5, 2_600.0), (6, 4.0), (7, 1.0), (16, 1.0), (17, 6_000.0)])
        }
        4 => tone(0.2, 2_600.0, 2_500.0, 0.002, 0.025),
        5 => tone(0.3, 1_100.0, 1_000.0, 0.003, 0.04).mix(&tone(0.3, 1_650.0, 1_600.0, 0.003, 0.03), 0.0, 0.5),
        // Tambourine: high jingles in three quick bursts.
        6 => {
            let jingle = |seed| {
                metal(0.6, &cymbal_partials(seed, 8, 5_000.0, 11_000.0), 6_000.0, 0.002, 0.09)
                    .mix(&noise(0.6, seed, FilterMode::HighPass, 8_000.0, 0.7, 0.002, 0.08, 0.6), 0.0, 0.4)
            };
            jingle(51).mix(&jingle(52), 0.018, 0.7).mix(&jingle(53), 0.032, 0.5)
        }
        // Triangle.
        7 => {
            let s = tone(2.0, 4_200.0, 4_200.0, 1.0, 0.8).mix(&tone(2.0, 6_350.0, 6_350.0, 1.0, 0.6), 0.0, 0.5);
            s.mix(&tone(2.0, 9_100.0, 9_100.0, 1.0, 0.4), 0.0, 0.25)
        }
        // FM metal hit.
        _ => {
            let p = [(1, 0.13), (10, 7.0), (11, 0.41), (12, 0.9), (14, 0.08), (15, 0.0), (4, 0.25), (5, 0.0)];
            room(hit("fm", &p, 76, 0.1, 0.8), 0.15)
        }
    };
    one_shot(b, Level::Peak)
}

fn tom(v: u32) -> Buf {
    let b = match v {
        0 => pad(5, 0.0, 1.2, 1.0),
        1 => pad(5, 5.0, 1.1, 0.9),
        2 => pad(7, 0.0, 1.1, 0.8),
        3 => tone(1.2, 180.0, 70.0, 0.06, 0.35).mix(&noise(0.1, 61, FilterMode::BandPass, 1_500.0, 1.0, 0.0005, 0.01, 0.0), 0.0, 0.2),
        4 => tone(1.0, 320.0, 140.0, 0.05, 0.25).mix(&noise(0.1, 62, FilterMode::BandPass, 2_000.0, 1.0, 0.0005, 0.01, 0.0), 0.0, 0.2),
        _ => verb(pad(5, -3.0, 1.4, 1.0), 0.0, 0.5, 1.4, 6_000.0, 0.25),
    };
    one_shot(b, Level::Peak)
}

/// Crash or ride: inharmonic metal plus air, a fast bright layer and a slow body.
fn cymbal(seed: u32, secs: f32, bright_tau: f32, body_tau: f32, hp: f32) -> Buf {
    let partials = cymbal_partials(seed, 14, 300.0, 9_000.0);
    let body = metal(secs, &partials, hp, 0.002, body_tau);
    let bright = noise(secs, seed + 1, FilterMode::HighPass, 6_000.0, 0.7, 0.001, bright_tau, 0.8);
    let wash = noise(secs, seed + 2, FilterMode::BandPass, 5_000.0, 0.5, 0.004, body_tau * 0.8, 0.9);
    body.mix(&bright, 0.0, 0.6).mix(&wash, 0.0, 0.35)
}

fn cymbals(v: u32) -> Buf {
    let b = match v {
        0 => room(cymbal(71, 4.0, 0.5, 1.3, 3_000.0), 0.1),
        1 => eq(cymbal(73, 4.5, 0.4, 1.6, 2_000.0), &[(16, 1.0), (17, 8_000.0)]),
        2 => crush(sat(cymbal(75, 3.0, 0.4, 0.9, 2_500.0), 1.0, 12.0, 0.0).gain(0.5), 10.0, 2.0, 0.6),
        // Ride: metal and a bell ping.
        3 => {
            let ping = tone(3.0, 3_150.0, 3_150.0, 1.0, 0.5).mix(&tone(3.0, 4_730.0, 4_730.0, 1.0, 0.3), 0.0, 0.6);
            cymbal(77, 4.0, 0.15, 2.0, 4_000.0).gain(0.6).mix(&ping, 0.0, 0.25)
        }
        4 => {
            let ping = tone(3.0, 2_550.0, 2_550.0, 1.0, 0.9).mix(&tone(3.0, 3_820.0, 3_820.0, 1.0, 0.7), 0.0, 0.7);
            ping.mix(&cymbal(79, 3.0, 0.08, 1.2, 5_000.0), 0.0, 0.25)
        }
        _ => cymbal(81, 1.6, 0.2, 0.35, 3_500.0),
    };
    one_shot(b, Level::Peak)
}

// --- Basses ---------------------------------------------------------------------

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// MIDI note name: 60 → C4.
pub fn note_name(note: u8) -> String {
    format!("{}{}", NOTE_NAMES[note as usize % 12], note as i32 / 12 - 1)
}

/// 808: the kick instrument with a long, tuned tail (the tail sits on `note`).
fn bass_808(note: u8, style: u32) -> Buf {
    // The kick plays midi_to_hz(note − 27).
    let key = note + 27;
    let b = match style {
        0 => {
            let p = [(1, 24.0), (2, 0.05), (4, 2.4), (5, 0.12), (7, 0.7), (8, 0.15), (9, 4_000.0)];
            sat(hit("kick", &p, key, 0.1, 3.5), 0.0, 2.0, 0.0)
        }
        1 => {
            let p = [(1, 26.0), (2, 0.05), (4, 2.0), (5, 0.3), (7, 0.7), (8, 0.9), (9, 7_000.0)];
            let b = sat(hit("kick", &p, key, 0.1, 3.0), 1.0, 16.0, 1.0).gain(0.4);
            fx(b, "char_saturation", &[(0, 0.0), (1, 8.0), (2, 1.0), (3, 120.0), (5, -6.0), (6, 4.0), (9, -6.0)])
        }
        _ => {
            let p = [(1, 30.0), (2, 0.04), (4, 0.7), (5, 0.4), (7, 0.6), (8, 0.4), (9, 6_000.0)];
            punch(hit("kick", &p, key, 0.1, 1.2), 0.3, 0.0)
        }
    };
    one_shot(eq(b, &[(0, 1.0), (1, 20.0)]), Level::Peak)
}

fn synth_bass(v: u32) -> (u8, Buf) {
    let (note, b) = match v {
        // Reese.
        0 => {
            let p = [(0, 0.0), (1, 0.6), (2, 1.0), (3, 28.0), (4, 0.3), (5, 0.5), (10, 900.0), (12, 6.0), (21, 1.0), (22, 1.0), (23, 0.2)];
            (29, fx(hit("wavetable", &p, 29, 1.6, 2.0), "chorus", &[(1, 0.3), (2, 0.4), (5, 0.25)]))
        }
        // Acid.
        1 => {
            let p = [(1, 280.0), (2, 0.88), (3, 0.85), (4, 0.3), (7, 0.5)];
            (33, hit("bass303", &p, 33, 0.35, 0.8))
        }
        // Sub sine.
        2 => {
            let p = [(0, 0.0), (2, 16_000.0), (4, 0.0), (6, 0.005), (8, 1.0), (9, 0.2)];
            (36, hit("pulse", &p, 36, 1.2, 1.6))
        }
        // FM pluck bass.
        3 => {
            let p = [(10, 1.0), (12, 0.75), (14, 0.15), (15, 0.05), (4, 0.6), (5, 0.3)];
            (28, hit("fm", &p, 28, 0.5, 1.0))
        }
        // Growl.
        4 => {
            let p = [(0, 1.0), (1, 0.8), (2, 2.0), (3, 18.0), (5, 0.4), (10, 600.0), (11, 3.0), (12, 30.0), (31, 0.25), (32, 0.4)];
            let b = hit("wavetable", &p, 31, 1.0, 1.4);
            let b = fx(b, "autofilter", &[(0, 2.0), (1, 900.0), (2, 4.0), (4, 0.0), (5, 6.0), (6, 0.7), (10, 0.6)]);
            (31, fx(b, "char_saturation", &[(0, 2.0), (1, 14.0), (9, -9.0)]))
        }
        // Rubber.
        _ => {
            let p = [(0, 2.0), (1, 4.0), (2, 200.0), (3, 3.0), (4, 2_500.0), (5, 0.12), (7, 0.25), (8, 0.3), (9, 0.08)];
            (38, hit("pulse", &p, 38, 0.4, 0.8))
        }
    };
    (note, one_shot(eq(b, &[(0, 1.0), (1, 25.0)]), Level::Peak))
}

// --- Synths -----------------------------------------------------------------------

fn pluck(v: u32) -> (u8, Buf) {
    let (note, b) = match v {
        0 => (60, room(hit("pluck", &[(0, 0.5), (1, 2.0), (4, 0.6)], 60, 1.5, 2.2), 0.15)),
        1 => {
            let p = [(0, 1.0), (1, 9.0), (2, 300.0), (3, 2.0), (4, 6_000.0), (5, 0.15), (7, 0.25), (8, 0.0), (9, 0.2)];
            let b = fx_at(hit("pulse", &p, 69, 0.2, 0.6).tail(1.0), "delay", &[(0, 9.0), (2, 0.35), (6, 0.2)], 120.0);
            (69, verb(b, 1.0, 0.5, 1.5, 9_000.0, 0.15))
        }
        2 => {
            let p = [(10, 4.0), (12, 0.6), (14, 0.2), (15, 0.0), (4, 0.6), (5, 0.0), (6, 0.4)];
            (76, room(hit("fm", &p, 76, 0.3, 1.2), 0.2))
        }
        3 => (62, hit("pluck", &[(0, 0.8), (1, 1.5), (2, 0.6), (3, 0.08), (4, 0.2)], 62, 1.0, 1.8)),
        4 => {
            let p = [(10, 7.0), (12, 0.5), (14, 0.12), (15, 0.0), (4, 0.35), (5, 0.0), (6, 0.3)];
            (67, room(hit("fm", &p, 67, 0.3, 0.9), 0.12))
        }
        _ => (72, verb(hit("pluck", &[(0, 0.7), (1, 4.0), (5, 0.6)], 72, 3.0, 3.5), 0.0, 0.6, 2.0, 8_000.0, 0.22)),
    };
    (note, one_shot(b, Level::Loud(-11.0)))
}

fn pluck_chord(v: u32) -> Buf {
    let b = match v {
        // Square chord, C minor.
        0 => {
            let p = [(0, 2.0), (2, 600.0), (4, 4_000.0), (5, 0.12), (7, 0.2), (8, 0.0), (9, 0.15)];
            room(chord("pulse", &p, &[60, 63, 67, 72], 0.2, 0.8), 0.18)
        }
        // Future bass chord, F major 9.
        _ => {
            let p = [(2, 6.0), (3, 25.0), (4, 0.8), (10, 800.0), (12, 40.0), (21, 0.25), (22, 0.0), (23, 0.3), (31, 0.2), (32, 0.0)];
            verb(chord("wavetable", &p, &[53, 57, 60, 64, 67], 0.3, 0.9), 1.0, 0.6, 2.0, 9_000.0, 0.2)
        }
    };
    one_shot(b, Level::Loud(-11.0))
}

fn stab(v: u32) -> Buf {
    let b = match v {
        // House organ, C minor 7.
        0 => {
            let p = [(0, 2.0), (1, 3.0), (2, 2_500.0), (4, 2_000.0), (5, 0.1), (7, 0.25), (8, 0.0), (9, 0.12)];
            room(fx(chord("pulse", &p, &[48, 60, 63, 67, 70], 0.25, 0.7), "chorus", &[(5, 0.3)]), 0.2)
        }
        // Detroit chord, F minor 9.
        1 => {
            let p = [(0, 1.0), (1, 8.0), (2, 600.0), (4, 5_000.0), (5, 0.2), (7, 0.3), (8, 0.1), (9, 0.3)];
            let b = fx_at(chord("pulse", &p, &[53, 56, 60, 63, 67], 0.3, 0.9).tail(1.5), "delay", &[(0, 9.0), (2, 0.4), (6, 0.22)], 124.0);
            verb(b, 1.0, 0.5, 1.6, 8_000.0, 0.18)
        }
        // Brass, F major.
        2 => {
            let p = [(0, 1.0), (1, 6.0), (2, 500.0), (4, 4_000.0), (5, 0.25), (6, 0.02), (7, 0.3), (8, 0.6), (9, 0.15)];
            room(chord("pulse", &p, &[53, 57, 60, 65], 0.35, 0.9), 0.15)
        }
        // Dub chord, A minor.
        3 => {
            let p = [(0, 1.0), (2, 1_200.0), (4, 2_000.0), (5, 0.1), (7, 0.15), (8, 0.0), (9, 0.1)];
            let b = chord("pulse", &p, &[57, 60, 64, 69], 0.15, 0.4).tail(2.5);
            let b = fx_at(b, "delay", &[(0, 12.0), (2, 0.55), (4, 400.0), (5, 3_000.0), (6, 0.35)], 120.0);
            verb(b, 1.0, 0.6, 2.0, 6_000.0, 0.25)
        }
        // E-piano, D minor 9.
        4 => chord("epiano", &[(0, 0.6), (2, 0.4)], &[50, 53, 57, 60, 64], 0.6, 1.8),
        // Supersaw, C major.
        _ => {
            let p = [(2, 7.0), (3, 30.0), (4, 0.9), (10, 4_000.0), (12, 12.0), (21, 0.3), (22, 0.0), (23, 0.25)];
            verb(chord("wavetable", &p, &[48, 60, 64, 67, 72], 0.3, 0.8), 1.0, 0.6, 1.8, 10_000.0, 0.2)
        }
    };
    one_shot(b, Level::Loud(-11.0))
}

fn pad_sound(v: u32) -> Buf {
    let hold = 4.5;
    let b = match v {
        // Warm saw, C major 7.
        0 => {
            let p = [(2, 6.0), (3, 18.0), (4, 0.8), (10, 1_200.0), (12, 6.0), (20, 1.2), (21, 2.0), (22, 0.9), (23, 2.0)];
            let b = fx(chord("wavetable", &p, &[48, 55, 59, 64, 67], hold, 7.0), "chorus", &[(1, 0.4), (2, 0.5), (5, 0.4)]);
            verb(b, 0.0, 0.8, 3.0, 7_000.0, 0.3)
        }
        // Glass, A minor 9.
        1 => {
            let p = [(10, 3.0), (12, 0.3), (13, 0.8), (14, 2.0), (15, 0.5), (3, 0.8), (6, 2.0), (16, 2.0)];
            verb(chord("fm", &p, &[57, 64, 67, 71, 72], hold, 7.5), 2.0, 0.8, 4.0, 9_000.0, 0.35)
        }
        // Dark, D minor.
        2 => {
            let p = [(0, 1.0), (1, 12.0), (2, 500.0), (4, 300.0), (6, 1.5), (8, 1.0), (9, 2.0)];
            let b = chord("pulse", &p, &[38, 50, 53, 57], hold, 7.0);
            let b = fx(b, "autofilter", &[(1, 700.0), (2, 1.5), (5, 0.15), (6, 0.4)]);
            verb(b, 0.0, 0.9, 4.0, 5_000.0, 0.35)
        }
        // Shimmer, F major 7.
        3 => {
            let p = [(0, 1.0), (1, 0.3), (2, 5.0), (3, 14.0), (10, 3_000.0), (20, 1.0), (22, 0.8), (23, 2.0)];
            let b = chord("wavetable", &p, &[53, 60, 64, 69], hold, 7.5);
            fx(b, "shimmer", &[(0, 0.8), (1, 5.0), (2, 0.5), (5, 0.35)])
        }
        // Strings, G minor.
        4 => {
            let p = [(0, 1.0), (1, 12.0), (2, 2_500.0), (4, 600.0), (6, 0.6), (8, 0.9), (9, 1.2)];
            let b = fx(chord("pulse", &p, &[43, 55, 58, 62, 67], hold, 6.5), "chorus", &[(1, 0.6), (2, 0.6), (5, 0.5)]);
            verb(b, 0.0, 0.7, 2.5, 8_000.0, 0.3)
        }
        // Airy, E-flat major 9.
        _ => {
            let p = [(1, 0.15), (2, 7.0), (3, 22.0), (4, 1.0), (10, 6_000.0), (12, 0.0), (20, 1.8), (22, 1.0), (23, 2.5)];
            let b = chord("wavetable", &p, &[51, 58, 62, 65, 70], hold, 8.0);
            verb(eq(b, &[(0, 1.0), (1, 200.0), (14, 8_000.0), (15, 3.0)]), 2.0, 0.9, 5.0, 12_000.0, 0.4)
        }
    };
    soft_shot(b, Level::Rms(-14.0))
}

fn keys(v: u32) -> (String, Buf) {
    let (name, b) = match v {
        0 => ("EPiano - Cmaj9".into(), chord("epiano", &[(6, 0.4)], &[48, 52, 55, 59, 62], 2.5, 4.0)),
        1 => ("EPiano Bright - Am9".into(), chord("epiano", &[(0, 0.85), (2, 0.6), (6, 0.2)], &[45, 55, 59, 60, 64], 2.0, 3.5)),
        2 => {
            let p = [(10, 3.0), (11, 0.5), (12, 0.5), (14, 1.5), (15, 0.0), (4, 3.0), (5, 0.0), (6, 2.0)];
            (format!("Bell - {}", note_name(84)), room(hit("fm", &p, 84, 0.5, 4.0), 0.2))
        }
        _ => {
            let p = [(10, 1.0), (12, 0.25), (14, 0.4), (4, 1.2), (5, 0.4)];
            (format!("Soft Keys - {}", note_name(60)), chord("fm", &p, &[60], 1.5, 2.5))
        }
    };
    (name, one_shot(b, Level::Loud(-11.0)))
}

// --- FX ---------------------------------------------------------------------------

fn riser(v: u32) -> Buf {
    let b = match v {
        // Noise: a band-pass sweeping up, swelling in.
        0 => {
            let secs = 8.0;
            let (mut a, mut b) = (WhiteNoise::new(91), WhiteNoise::new(92));
            let (mut fa, mut fb) = (Svf::new(FilterMode::BandPass, 300.0, 1.5, SR), Svf::new(FilterMode::BandPass, 300.0, 1.5, SR));
            let s = synth(secs, |t| {
                let x = t / secs;
                let hz = 300.0 * (12_000.0f32 / 300.0).powf(x * x);
                fa.set_params(hz, 1.5, SR);
                fb.set_params(hz * 1.05, 1.5, SR);
                let g = x.powf(1.5);
                (fa.process(a.next_sample()) * g, fb.process(b.next_sample()) * g)
            });
            verb(s, 0.0, 0.8, 2.5, 10_000.0, 0.3)
        }
        // Supersaw climbing two octaves.
        1 => {
            let secs = 8.0;
            let detune = [-0.11, -0.06, -0.02, 0.0, 0.02, 0.06, 0.11];
            let mut phases = [0.0f32; 7];
            let (mut fl, mut fr) = (Svf::new(FilterMode::LowPass, 400.0, 1.0, SR), Svf::new(FilterMode::LowPass, 400.0, 1.0, SR));
            let s = synth(secs, |t| {
                let x = t / secs;
                let base = midi_to_hz(48.0 + 24.0 * x * x);
                let (mut l, mut r) = (0.0, 0.0);
                for (k, (p, d)) in phases.iter_mut().zip(detune).enumerate() {
                    *p = (*p + base * 2f32.powf(d / 12.0) / SR).fract();
                    let s = 2.0 * *p - 1.0;
                    let w = k as f32 / 6.0;
                    l += s * (1.0 - w);
                    r += s * w;
                }
                let hz = 400.0 * (12_000.0f32 / 400.0).powf(x);
                fl.set_params(hz, 1.2, SR);
                fr.set_params(hz, 1.2, SR);
                let g = x.powf(1.2) * 0.3;
                (fl.process(l) * g, fr.process(r) * g)
            });
            verb(s, 1.0, 0.7, 2.0, 10_000.0, 0.25)
        }
        // Snare roll at 124 BPM: quarters, eighths, sixteenths, thirty-seconds.
        _ => {
            let beat = 60.0 / 124.0;
            let s = snare(0);
            let mut out = Buf::new(frames(beat * 16.0));
            let mut t = 0.0;
            for (bar, div) in [1.0f32, 2.0, 4.0, 8.0].iter().enumerate() {
                let step = beat / div;
                let count = (4.0 * div) as usize;
                for i in 0..count {
                    let x = (bar as f32 * 4.0 + i as f32 / div) / 16.0;
                    out.mix_at(&s, frames(t), 0.25 + 0.75 * x * x);
                    t += step;
                }
            }
            let out = fx(out, "autofilter", &[(0, 1.0), (1, 200.0), (6, 0.0), (7, 0.0)]);
            room(out, 0.2)
        }
    };
    soft_shot(b, Level::Rms(-12.0))
}

fn impact(v: u32) -> Buf {
    let b = match v {
        // Boom: a very low, long kick into a hall.
        0 => {
            let k = hit("kick", &[(0, -10.0), (1, 36.0), (2, 0.12), (4, 3.0), (5, 0.5), (8, 0.6)], 60, 0.1, 4.0);
            verb(sat(k, 0.0, 6.0, 0.0), 0.0, 0.9, 4.0, 4_000.0, 0.25)
        }
        // Sub drop.
        1 => tone(3.5, 110.0, 32.0, 0.6, 1.2),
        // Cinematic hit: boom, crash and noise, big hall.
        2 => {
            let k = hit("kick", &[(0, -6.0), (1, 40.0), (2, 0.08), (4, 2.0), (5, 0.8), (8, 0.8)], 60, 0.1, 3.0);
            let b = k.mix(&cymbal(97, 3.0, 0.4, 1.0, 2_000.0), 0.0, 0.35).mix(&pad(3, -6.0, 2.0, 1.0), 0.0, 0.4);
            verb(b, 0.0, 1.0, 5.0, 6_000.0, 0.35)
        }
        // Lofi thud.
        _ => {
            let k = hit("kick", &[(0, -8.0), (1, 30.0), (2, 0.08), (4, 1.5), (8, 0.7), (9, 2_000.0)], 60, 0.1, 2.0);
            crush(k.mix(&noise(1.0, 99, FilterMode::LowPass, 800.0, 0.7, 0.001, 0.3, 0.5), 0.0, 0.3), 9.0, 4.0, 0.6)
        }
    };
    one_shot(b, Level::Peak)
}

fn sweep(v: u32) -> Buf {
    let b = match v {
        // Noise down.
        0 => {
            let secs = 4.0;
            let (mut a, mut b) = (WhiteNoise::new(101), WhiteNoise::new(102));
            let (mut fa, mut fb) = (Svf::new(FilterMode::LowPass, 12_000.0, 2.0, SR), Svf::new(FilterMode::LowPass, 12_000.0, 2.0, SR));
            let s = synth(secs, |t| {
                let x = t / secs;
                let hz = 12_000.0 * (150.0f32 / 12_000.0).powf(x);
                fa.set_params(hz, 2.0, SR);
                fb.set_params(hz * 0.95, 2.0, SR);
                let g = (1.0 - x).powf(0.8) * env(t, 0.05, 100.0);
                (fa.process(a.next_sample()) * g, fb.process(b.next_sample()) * g)
            });
            verb(s, 1.0, 0.6, 2.0, 8_000.0, 0.25)
        }
        // Noise up, short.
        1 => riser_short(),
        // Reverse crash.
        2 => cymbal(111, 3.0, 0.6, 1.0, 3_000.0).reverse(),
        // Laser: a fast falling sine with a little FM.
        _ => {
            let mut phase = 0.0f32;
            let mut mphase = 0.0f32;
            let s = synth(0.8, |t| {
                let f = 80.0 + 3_000.0 * (-t / 0.12).exp();
                mphase = (mphase + f * 1.5 / SR).fract();
                phase = (phase + f * (1.0 + 0.3 * (TAU * mphase).sin()) / SR).fract();
                let s = (TAU * phase).sin() * env(t, 0.001, 0.25);
                (s, s)
            });
            fx_at(s.tail(1.0), "delay", &[(0, 8.0), (2, 0.4), (3, 1.0), (6, 0.3)], 124.0)
        }
    };
    soft_shot(b, Level::Rms(-12.0))
}

fn riser_short() -> Buf {
    let secs = 2.0;
    let mut a = WhiteNoise::new(103);
    let mut f = Svf::new(FilterMode::HighPass, 200.0, 1.0, SR);
    let s = synth(secs, |t| {
        let x = t / secs;
        f.set_params(200.0 * (8_000.0f32 / 200.0).powf(x), 1.5, SR);
        let v = f.process(a.next_sample()) * x * x;
        (v, v)
    });
    fx(s.tail(0.5), "chorus", &[(2, 0.6), (4, 1.0), (5, 0.5)])
}

// --- Loops --------------------------------------------------------------------------

/// A lane of a beat: a sound, a step pattern and the steps per beat.
/// Pattern: `X` accent, `x` normal, `o` ghost, `.` rest; repeats to fill the loop.
struct Lane<'a> {
    sound: &'a Buf,
    pattern: &'a str,
    per_beat: usize,
    gain: f32,
    pan: f32,
}

fn lane<'a>(sound: &'a Buf, pattern: &'a str, gain: f32) -> Lane<'a> {
    Lane { sound, pattern, per_beat: 4, gain, pan: 0.0 }
}

impl Lane<'_> {
    fn per(mut self, per_beat: usize) -> Self {
        self.per_beat = per_beat;
        self
    }

    fn pan(mut self, pan: f32) -> Self {
        self.pan = pan;
        self
    }
}

/// Sequences the lanes over `bars` bars of 4/4. Tails wrap to the start so the
/// loop plays seamlessly. `swing` delays every second sixteenth (0 → straight).
fn beat(bpm: f32, bars: usize, swing: f32, lanes: &[Lane]) -> Buf {
    let beat = 60.0 / bpm;
    let len = frames(beat * 4.0 * bars as f32);
    let mut out = Buf::new(len);
    for lane in lanes {
        let pattern: Vec<char> = lane.pattern.chars().filter(|c| !c.is_whitespace()).collect();
        let steps = lane.per_beat * 4 * bars;
        let step = beat / lane.per_beat as f32;
        let sound = lane.sound.clone().pan(lane.pan);
        for s in 0..steps {
            let vel = match pattern[s % pattern.len()] {
                'X' => 1.0,
                'x' => 0.78,
                'o' => 0.42,
                _ => continue,
            };
            let mut t = s as f32 * step;
            if lane.per_beat == 4 && s % 2 == 1 {
                t += swing * step;
            }
            out.mix_at(&sound, frames(t), vel * lane.gain);
        }
    }
    wrap(out, len)
}

/// Folds everything past `len` back onto the start.
fn wrap(mut b: Buf, len: usize) -> Buf {
    for i in len..b.len() {
        b.l[i % len] += b.l[i];
        b.r[i % len] += b.r[i];
    }
    b.l.truncate(len);
    b.r.truncate(len);
    b
}

/// Bus processing on a loop: runs over two passes so effects enter the loop
/// already warmed up, then keeps the second pass (seamless at the loop point).
fn bus(b: Buf, chain: impl Fn(Buf) -> Buf) -> Buf {
    let len = b.len();
    let mut twice = b.clone();
    twice.mix_at(&b, len, 1.0);
    let mut out = chain(twice.tail(1.0));
    // Whatever rings past the second pass folds onto its start.
    let tail = Buf { l: out.l.split_off(2 * len), r: out.r.split_off(2 * len) };
    let mut second = Buf { l: out.l.split_off(len), r: out.r.split_off(len) };
    for i in 0..tail.len().min(len) {
        second.l[i] += tail.l[i];
        second.r[i] += tail.r[i];
    }
    second
}

/// Finishes a loop: glue, limiter at −1 dBFS, loudness target, peak ceiling.
fn finish_loop(b: Buf, bpm: f32) -> Buf {
    let b = bus(b, |x| {
        let x = fx_at(x, "bus_comp", &[], bpm);
        let rms = x.rms_max().max(1e-6);
        let x = x.gain(db(-9.0) / rms);
        fx_at(x, "limiter", &[(0, 0.0), (1, -1.2), (2, 0.08)], bpm)
    });
    level(b, Level::Rms(-10.0))
}

fn loop_house(v: u32) -> Buf {
    let bpm = 124.0;
    let (k, cl, ch, oh, sh, rm) = (kick_house(0), clap(5), hat_closed(0), hat_open(0), shaker(0), rim(0));
    let b = match v {
        0 => beat(bpm, 4, 0.08, &[
            lane(&k, "X...X...X...X...", 1.0),
            lane(&cl, "....X.......X...", 0.55),
            lane(&oh, "..x...x...x...x.", 0.3).pan(0.15),
            lane(&ch, "x.o.x.o.x.o.x.oo", 0.22).pan(-0.15),
            lane(&sh, "oxoxoxoxoxoxoxox", 0.12).pan(0.3),
        ]),
        // Top loop: no kick.
        _ => beat(bpm, 4, 0.1, &[
            lane(&cl, "....X.......X...", 0.6),
            lane(&oh, "..x...x...x...x.", 0.35).pan(0.15),
            lane(&ch, "xoxoxoxoxoxoxoxx", 0.25).pan(-0.15),
            lane(&rm, "...x..x....x.x..", 0.25).pan(-0.3),
            lane(&sh, "oxoxoxoxoxoxoxox", 0.15).pan(0.3),
        ]),
    };
    finish_loop(b, bpm)
}

fn loop_hiphop(v: u32) -> Buf {
    let bpm = 90.0;
    let b = match v {
        0 => {
            let (k, s, h) = (kick_lofi(0), snare(1), hat_closed(1));
            beat(bpm, 4, 0.18, &[
                lane(&k, "X......x..X.....X......x.xX.....", 1.0),
                lane(&s, "....X.......X.......X.......X..o", 0.75),
                lane(&h, "x.x.x.xox.x.x.x.", 0.25).pan(0.1),
            ])
        }
        _ => {
            let (k, s, h, r) = (kick_lofi(1), snare(3), hat_closed(4), rim(3));
            let b = beat(bpm, 4, 0.22, &[
                lane(&k, "X.....x...X.....X.....x..XX.....", 1.0),
                lane(&s, "....X.......X...", 0.7),
                lane(&h, "xoxoxoxoxoxoxoxo", 0.25).pan(0.15),
                lane(&r, ".......x.....x..", 0.3).pan(-0.25),
            ]);
            eq(b, &[(16, 1.0), (17, 9_000.0)])
        }
    };
    finish_loop(b, bpm)
}

fn loop_trap(v: u32) -> Buf {
    let bpm = 140.0;
    let (k, s, h) = (kick_808(1), snare(2), hat_closed(3));
    // Hats in 32nds: eighths with rolls.
    let rolls = "x...x...x...x...x...x.x.xxxxx...x...x...x...x...x...x...x.x.x.x.";
    let b = match v {
        0 => beat(bpm, 4, 0.0, &[
            lane(&k, "X.........X.....X.....X...X.....", 1.0),
            lane(&s, "........X.......", 0.7),
            lane(&h, rolls, 0.28).per(8).pan(0.1),
        ]),
        _ => beat(bpm, 4, 0.0, &[
            lane(&h, rolls, 0.4).per(8).pan(0.1),
            lane(&h, "......x.......x.", 0.15).pan(-0.3),
            lane(&clap(2), "........X.......", 0.5),
        ]),
    };
    finish_loop(b, bpm)
}

fn loop_dnb(v: u32) -> Buf {
    let bpm = 174.0;
    let (k, s, h, oh) = (kick_house(2), snare(6), hat_closed(1), hat_open(3));
    let b = match v {
        // Two-step.
        0 => beat(bpm, 4, 0.0, &[
            lane(&k, "X.........X.....", 1.0),
            lane(&s, "....X.......X...", 0.8),
            lane(&s, "......o......o..", 0.6),
            lane(&h, "x.x.x.x.x.x.x.xx", 0.25).pan(0.1),
            lane(&oh, "..............x.", 0.2).pan(-0.2),
        ]),
        // Roller.
        _ => beat(bpm, 4, 0.0, &[
            lane(&k, "X.........X..x..X.........X.....", 1.0),
            lane(&s, "....X..o....X...", 0.8),
            lane(&h, "xoxoxoxoxoxoxoxo", 0.3).pan(0.1),
            lane(&shaker(2), "oxoxoxoxoxoxoxox", 0.18).pan(-0.25),
        ]),
    };
    finish_loop(b, bpm)
}

/// Breakcore: a synthesized break, then chopped, stuttered and reversed by slices.
fn loop_breakcore(v: u32) -> Buf {
    let bpm = 180.0;
    let (k, s, h, c) = (kick_lofi(0), snare(4), hat_closed(0), cymbals(5));
    // An amen-like bar.
    let source = beat(bpm, 1, 0.0, &[
        lane(&k, "X.X.......XX....", 1.0),
        lane(&s, "....X..o.o..X..o", 0.85),
        lane(&h, "x.x.x.x.x.x.x.x.", 0.3).pan(0.1),
    ]);
    let slice = frames(60.0 / bpm / 4.0);
    let take = |i: usize| -> Buf {
        let a = (i % 16) * slice;
        Buf { l: source.l[a..a + slice].to_vec(), r: source.r[a..a + slice].to_vec() }
    };
    // Per sixteenth: slice index, then an operation: '-' plain, 's' stutter (4 × 1/64), 'r' reversed.
    let plan: &[(usize, char)] = match v {
        0 => &[
            (0, '-'), (1, '-'), (2, '-'), (3, '-'), (4, '-'), (5, '-'), (6, 's'), (7, '-'),
            (8, '-'), (9, '-'), (10, '-'), (11, '-'), (12, '-'), (12, 's'), (14, 'r'), (15, 's'),
            (0, '-'), (1, '-'), (4, '-'), (4, 's'), (2, '-'), (3, '-'), (12, '-'), (7, '-'),
            (10, '-'), (11, '-'), (4, 'r'), (9, '-'), (12, 's'), (12, 's'), (12, 's'), (15, '-'),
        ],
        _ => &[
            (0, '-'), (2, '-'), (4, '-'), (4, 's'), (10, '-'), (11, '-'), (12, '-'), (13, 'r'),
            (0, 's'), (0, '-'), (4, '-'), (7, '-'), (8, '-'), (12, 's'), (12, '-'), (15, 'r'),
            (0, '-'), (1, '-'), (2, '-'), (3, '-'), (4, 's'), (4, 's'), (6, '-'), (7, '-'),
            (10, '-'), (10, 's'), (12, '-'), (12, 'r'), (4, 's'), (12, 's'), (4, 's'), (12, 's'),
        ],
    };
    let bars = 2;
    let mut out = Buf::new(slice * 16 * bars);
    for (step, &(idx, op)) in plan.iter().cycle().take(16 * bars).enumerate() {
        let mut piece = take(idx);
        match op {
            's' => {
                let q = slice / 4;
                for j in 0..slice {
                    piece.l[j] = piece.l[j % q] * if j % q > q - 32 { ((q - j % q) as f32 / 32.0).min(1.0) } else { 1.0 };
                    piece.r[j] = piece.r[j % q] * if j % q > q - 32 { ((q - j % q) as f32 / 32.0).min(1.0) } else { 1.0 };
                }
            }
            'r' => piece = piece.reverse(),
            _ => {}
        }
        // Short fades at slice edges: no clicks between chops.
        let piece = piece.fade_in(0.000_7).fade_out(0.001_5);
        out.mix_at(&piece, step * slice, 1.0);
    }
    let out = out.mix(&c, 0.0, 0.35);
    let len = slice * 16 * bars;
    let out = wrap(out, len);
    let out = bus(out, |x| sat(x, 1.0, 8.0, 1.0).gain(0.6));
    finish_loop(out, bpm)
}

// --- Catalogue ------------------------------------------------------------------------

/// One file of the library.
pub struct LibraryItem {
    /// Folder relative to the library root, `/`-separated.
    pub folder: &'static str,
    /// File name without extension.
    pub name: String,
    render: Box<dyn Fn() -> Buf + Send + Sync>,
}

impl LibraryItem {
    pub fn relative_path(&self) -> PathBuf {
        let mut p = PathBuf::new();
        for part in self.folder.split('/') {
            p.push(part);
        }
        p.join(format!("{}.wav", self.name))
    }

    /// The finished sound.
    pub fn render(&self) -> Buf {
        (self.render)()
    }

    /// The file's bytes: 24-bit / 48 kHz stereo WAV.
    pub fn wav_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        encode_wav(&mut out, &self.render().interleaved(), LIBRARY_SAMPLE_RATE, BitDepth::Pcm24).expect("writing to memory");
        out
    }
}

fn numbered(prefix: &str, style: &str, i: u32) -> String {
    format!("{prefix} - {style} {:02}", i + 1)
}

/// A family of sounds: variant number → finished sound.
type Variant = fn(u32) -> Buf;

/// Every file of the library, in a stable order.
pub fn library_items() -> Vec<LibraryItem> {
    let mut v = Vec::new();
    let mut add = |folder: &'static str, name: String, f: Box<dyn Fn() -> Buf + Send + Sync>| {
        v.push(LibraryItem { folder, name, render: f });
    };
    let kicks: [(&str, u32, Variant); 5] =
        [("Punchy House", 3, kick_house), ("808 Long", 3, kick_808), ("Techno", 2, kick_techno), ("Hardstyle", 2, kick_hardstyle), ("Lofi", 2, kick_lofi)];
    for (style, count, f) in kicks {
        for i in 0..count {
            add("Drums/Kicks", numbered("Kick", style, i), Box::new(move || f(i)));
        }
    }
    let snares = ["Tight", "Fat", "Trap", "Lofi", "Breakbeat", "Roomy", "DnB", "Snappy 909"];
    for (i, s) in snares.iter().enumerate() {
        add("Drums/Snares", numbered("Snare", s, 0), Box::new(move || snare(i as u32)));
    }
    let claps = ["Tight", "Big Room", "Trap Stack", "Wide", "Lofi", "House"];
    for (i, s) in claps.iter().enumerate() {
        add("Drums/Claps", numbered("Clap", s, 0), Box::new(move || clap(i as u32)));
    }
    let rims = ["Dry", "Room", "Woody", "Lofi"];
    for (i, s) in rims.iter().enumerate() {
        add("Drums/Rims", numbered("Rim", s, 0), Box::new(move || rim(i as u32)));
    }
    let closed = ["Tight", "Crisp", "Dark Metal", "Trap", "Lofi", "Soft"];
    for (i, s) in closed.iter().enumerate() {
        add("Drums/Hats", numbered("Closed Hat", s, 0), Box::new(move || hat_closed(i as u32)));
    }
    let open = ["Classic", "Long", "Trashy", "Dark", "Sizzle"];
    for (i, s) in open.iter().enumerate() {
        add("Drums/Hats", numbered("Open Hat", s, 0), Box::new(move || hat_open(i as u32)));
    }
    let shakers = ["Soft", "Bright", "Double", "Long"];
    for (i, s) in shakers.iter().enumerate() {
        add("Drums/Shakers", numbered("Shaker", s, 0), Box::new(move || shaker(i as u32)));
    }
    let percs = ["Conga High", "Conga Low", "Bongo", "Cowbell", "Clave", "Woodblock", "Tambourine", "Triangle", "FM Metal"];
    for (i, s) in percs.iter().enumerate() {
        add("Drums/Percussion", numbered("Perc", s, 0), Box::new(move || perc(i as u32)));
    }
    let toms = ["Low", "Mid", "High", "Low Electronic", "High Electronic", "Floor Room"];
    for (i, s) in toms.iter().enumerate() {
        add("Drums/Toms", numbered("Tom", s, 0), Box::new(move || tom(i as u32)));
    }
    let cym = [("Crash", "Bright"), ("Crash", "Dark"), ("Crash", "Trashy"), ("Ride", "Classic"), ("Ride", "Bell"), ("Splash", "Short")];
    for (i, &(kind, s)) in cym.iter().enumerate() {
        add("Drums/Cymbals", numbered(kind, s, 0), Box::new(move || cymbals(i as u32)));
    }
    let loops = [
        ("Loops/House", "House", 124, loop_house as Variant, ["Beat 01", "Top Loop 01"]),
        ("Loops/Hip-Hop", "Hip-Hop", 90, loop_hiphop, ["Boom Bap 01", "Lofi 01"]),
        ("Loops/Trap", "Trap", 140, loop_trap, ["Beat 01", "Hat Rolls 01"]),
        ("Loops/DnB", "DnB", 174, loop_dnb, ["Two-Step 01", "Roller 01"]),
        ("Loops/Breakcore", "Breakcore", 180, loop_breakcore, ["Chop 01", "Stutter 01"]),
    ];
    for (folder, genre, bpm, f, names) in loops {
        for (i, name) in names.iter().enumerate() {
            add(folder, format!("{genre} {name} - {bpm} BPM"), Box::new(move || f(i as u32)));
        }
    }
    for (style, name, notes) in [
        (0, "Clean", &[24u8, 26, 28, 29, 31, 33][..]),
        (1, "Distorted", &[24, 28, 31][..]),
        (2, "Punchy", &[29, 34][..]),
    ] {
        for &note in notes {
            add("Bass/808", format!("808 - {name} - {}", note_name(note)), Box::new(move || bass_808(note, style)));
        }
    }
    let basses = ["Reese", "Acid", "Sub Sine", "FM Pluck", "Growl", "Rubber"];
    for (i, s) in basses.iter().enumerate() {
        let note = synth_bass_note(i as u32);
        add("Bass/Synth", format!("Bass - {s} - {}", note_name(note)), Box::new(move || synth_bass(i as u32).1));
    }
    let plucks = ["Nylon", "Bright Saw", "Glass FM", "Koto", "Mallet", "Harp"];
    for (i, s) in plucks.iter().enumerate() {
        let note = pluck_note(i as u32);
        add("Synths/Plucks", format!("Pluck - {s} - {}", note_name(note)), Box::new(move || pluck(i as u32).1));
    }
    add("Synths/Plucks", "Pluck Chord - Square - Cm".into(), Box::new(|| pluck_chord(0)));
    add("Synths/Plucks", "Pluck Chord - Future - Fmaj9".into(), Box::new(|| pluck_chord(1)));
    let stabs = ["House Organ - Cm7", "Detroit - Fm9", "Brass - F", "Dub - Am", "EPiano - Dm9", "Supersaw - C"];
    for (i, s) in stabs.iter().enumerate() {
        add("Synths/Stabs", format!("Stab - {s}"), Box::new(move || stab(i as u32)));
    }
    let pads = ["Warm Saw - Cmaj7", "Glass - Am9", "Dark - Dm", "Shimmer - Fmaj7", "Strings - Gm", "Airy - Ebmaj9"];
    for (i, s) in pads.iter().enumerate() {
        add("Synths/Pads", format!("Pad - {s}"), Box::new(move || pad_sound(i as u32)));
    }
    for i in 0..4 {
        add("Synths/Keys", format!("Keys - {}", keys_name(i)), Box::new(move || keys(i).1));
    }
    let risers = ["Noise 8s", "Supersaw 8s", "Snare Roll 124 BPM"];
    for (i, s) in risers.iter().enumerate() {
        add("FX/Risers", format!("Riser - {s}"), Box::new(move || riser(i as u32)));
    }
    let impacts = ["Boom", "Sub Drop", "Cinematic Hit", "Lofi Thud"];
    for (i, s) in impacts.iter().enumerate() {
        add("FX/Impacts", format!("Impact - {s}"), Box::new(move || impact(i as u32)));
    }
    let sweeps = ["Noise Down 4s", "Noise Up 2s", "Reverse Crash", "Laser Down"];
    for (i, s) in sweeps.iter().enumerate() {
        add("FX/Sweeps", format!("Sweep - {s}"), Box::new(move || sweep(i as u32)));
    }
    v
}

/// Relative paths of every file, in catalogue order (drums, loops, basses,
/// synths, FX): the order the browser lists them in.
pub fn library_order() -> Vec<PathBuf> {
    library_items().iter().map(LibraryItem::relative_path).collect()
}

fn synth_bass_note(v: u32) -> u8 {
    [29, 33, 36, 28, 31, 38][v as usize % 6]
}

fn pluck_note(v: u32) -> u8 {
    [60, 69, 76, 62, 67, 72][v as usize % 6]
}

fn keys_name(v: u32) -> String {
    match v {
        0 => "EPiano - Cmaj9".into(),
        1 => "EPiano Bright - Am9".into(),
        2 => format!("Bell - {}", note_name(84)),
        _ => format!("Soft Keys - {}", note_name(60)),
    }
}

// --- Writing --------------------------------------------------------------------------

/// True if `dir` holds a complete library of the current version.
pub fn library_is_current(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(LIBRARY_MARKER)).is_ok_and(|s| s.trim() == LIBRARY_VERSION.to_string())
}

/// Renders the whole library into `dir` on `threads` threads. `progress(done,
/// total)` is called after each file. Returns the number of files written.
pub fn generate_library(dir: &Path, threads: usize, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<usize, String> {
    let items = library_items();
    let total = items.len();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let error: Arc<Mutex<Option<String>>> = Arc::default();
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, 16) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    if error.lock().is_ok_and(|e| e.is_some()) {
                        break;
                    }
                    let path = dir.join(item.relative_path());
                    let result = path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(&path, item.wav_bytes()));
                    if let Err(e) = result {
                        if let Ok(mut slot) = error.lock() {
                            slot.get_or_insert(format!("{}: {e}", path.display()));
                        }
                        break;
                    }
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                }
            });
        }
    });
    if let Some(e) = error.lock().ok().and_then(|mut e| e.take()) {
        return Err(e);
    }
    let marker = dir.join(LIBRARY_MARKER);
    std::fs::write(&marker, format!("{LIBRARY_VERSION}\n")).map_err(|e| format!("{}: {e}", marker.display()))?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_clean() {
        let items = library_items();
        assert!(items.len() >= 120, "{} items", items.len());
        let mut paths: Vec<PathBuf> = items.iter().map(LibraryItem::relative_path).collect();
        paths.sort();
        let n = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), n, "duplicate file names");
        for item in &items {
            assert!(!item.name.contains("  ") && !item.name.contains('/'), "{}", item.name);
        }
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(34), "A#1");
    }

    #[test]
    fn every_sound_is_audible_unclipped_and_declicked() {
        for item in library_items() {
            let b = item.render();
            let name = &item.name;
            assert!(b.len() > frames(0.05), "{name}: too short");
            let peak = b.peak();
            assert!(peak <= CEILING * 1.000_1, "{name}: peak {peak}");
            assert!(peak > db(-10.0), "{name}: peak {peak} too low");
            assert!(b.rms_max() > db(-40.0), "{name}: nearly silent");
            assert!(b.l.iter().chain(&b.r).all(|s| s.is_finite()), "{name}: not finite");
            if !item.folder.starts_with("Loops") {
                let end = b.len() - 1;
                for s in [b.l[0], b.r[0], b.l[end], b.r[end]] {
                    assert!(s.abs() < 0.01, "{name}: click at an edge ({s})");
                }
            }
        }
    }

    #[test]
    fn rendering_is_deterministic() {
        let items = library_items();
        for item in &items {
            assert!(item.wav_bytes() == item.wav_bytes(), "{}: differs between two renders", item.name);
        }
    }

    #[test]
    fn loops_have_the_tempo_in_name_and_whole_bars() {
        for item in library_items().iter().filter(|i| i.folder.starts_with("Loops")) {
            let bpm: f32 = item.name.rsplit(" - ").next().and_then(|s| s.strip_suffix(" BPM")).and_then(|s| s.parse().ok()).expect("tempo");
            let bars = item.render().len() as f32 / (SR * 240.0 / bpm);
            assert!((bars - bars.round()).abs() < 0.01 && bars >= 1.0, "{}: {bars} bars", item.name);
        }
    }
}
