//! Ponzik Wave : synthé à tables d'ondes avec unisson et sous-oscillateur.
//!
//! Les tables sont calculées à la construction et à chaque changement de
//! fréquence d'échantillonnage : chaque image est dessinée « naïvement », passée
//! en fréquence par FFT, puis déclinée par octave en retirant les harmoniques
//! qui dépasseraient Nyquist. Pas de repliement, même tout en haut du clavier.

use std::f64::consts::TAU;

use ponzik_dsp::env::{Adsr, AdsrParams};
use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::{balance_gains, db_to_gain, gain_to_db};

use super::common::{Glide, fast_sin, glide_coef, pick_voice, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};

const TABLE_LEN: usize = 2048;
/// Une case de garde en fin de table pour l'interpolation.
const STRIDE: usize = TABLE_LEN + 1;
const LEVELS: usize = 10;
/// Fondamentale maximale servie par le niveau 0 ; chaque niveau monte d'une octave.
const LEVEL0_HZ: f32 = 40.0;
const VOICES: usize = 8;
const MAX_UNISON: usize = 8;
/// Désaccord d'unisson maximal (100 cents), pris en compte pour choisir le niveau.
const MAX_DETUNE_RATIO: f32 = 1.06;
/// Le filtre est recalculé tous les `CONTROL_RATE` échantillons.
const CONTROL_RATE: u32 = 16;

const SETS: &[&str] = &["Basique", "Harmonique"];
const UNISON: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8"];

const P_SET: u32 = 0;
const P_POSITION: u32 = 1;
const P_UNISON: u32 = 2;
const P_DETUNE: u32 = 3;
const P_SPREAD: u32 = 4;
const P_SUB: u32 = 5;
const P_CUTOFF: u32 = 10;
const P_RESO: u32 = 11;
const P_FENV: u32 = 12;
const P_KEYTRACK: u32 = 13;
const P_ATTACK: u32 = 20;
const P_DECAY: u32 = 21;
const P_SUSTAIN: u32 = 22;
const P_RELEASE: u32 = 23;
const P_FATTACK: u32 = 30;
const P_FDECAY: u32 = 31;
const P_FSUSTAIN: u32 = 32;
const P_FRELEASE: u32 = 33;
const P_GLIDE: u32 = 40;
const P_VOLUME: u32 = 41;

static PARAMS: [ParamInfo; 20] = [
    ParamInfo::choice(P_SET, "Table", SETS, 0).group("Oscillateur"),
    ParamInfo::new(P_POSITION, "Position", 0.0, 1.0, 0.5, Unit::Percent).group("Oscillateur"),
    ParamInfo::new(P_SUB, "Sous-oscillateur", 0.0, 1.0, 0.0, Unit::Percent).group("Oscillateur"),
    ParamInfo::choice(P_UNISON, "Voix", UNISON, 3).group("Unisson"),
    ParamInfo::new(P_DETUNE, "Désaccord", 0.0, 100.0, 12.0, Unit::Cents).group("Unisson"),
    ParamInfo::new(P_SPREAD, "Largeur", 0.0, 1.0, 0.5, Unit::Percent).group("Unisson"),
    ParamInfo::new(P_CUTOFF, "Coupure", 20.0, 20_000.0, 2_000.0, Unit::Hz).log().group("Filtre"),
    ParamInfo::new(P_RESO, "Résonance", 0.5, 10.0, 0.9, Unit::None).log().group("Filtre"),
    ParamInfo::new(P_FENV, "Enveloppe", -60.0, 60.0, 24.0, Unit::Semitones).group("Filtre"),
    ParamInfo::new(P_KEYTRACK, "Suivi de clavier", 0.0, 1.0, 0.5, Unit::Percent).group("Filtre"),
    ParamInfo::new(P_ATTACK, "Attaque", 0.001, 4.0, 0.005, Unit::Seconds).log().group("Amplitude"),
    ParamInfo::new(P_DECAY, "Déclin", 0.005, 8.0, 0.3, Unit::Seconds).log().group("Amplitude"),
    ParamInfo::new(P_SUSTAIN, "Maintien", 0.0, 1.0, 0.7, Unit::Percent).group("Amplitude"),
    ParamInfo::new(P_RELEASE, "Relâche", 0.005, 8.0, 0.25, Unit::Seconds).log().group("Amplitude"),
    ParamInfo::new(P_FATTACK, "Attaque", 0.001, 4.0, 0.002, Unit::Seconds).log().group("Enveloppe filtre"),
    ParamInfo::new(P_FDECAY, "Déclin", 0.005, 8.0, 0.4, Unit::Seconds).log().group("Enveloppe filtre"),
    ParamInfo::new(P_FSUSTAIN, "Maintien", 0.0, 1.0, 0.2, Unit::Percent).group("Enveloppe filtre"),
    ParamInfo::new(P_FRELEASE, "Relâche", 0.005, 8.0, 0.3, Unit::Seconds).log().group("Enveloppe filtre"),
    ParamInfo::new(P_GLIDE, "Portamento", 0.0, 2.0, 0.0, Unit::Seconds).group("Global"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -10.0, Unit::Db).group("Global"),
];

/// Jeu de tables : `frames` images × `LEVELS` niveaux d'octave.
struct TableSet {
    frames: usize,
    data: Vec<f32>,
}

impl TableSet {
    /// `shape(image, t)` : forme naïve sur une période, `t` dans [0, 1).
    fn build(frames: usize, shape: fn(usize, f64) -> f64, sample_rate: f32) -> Self {
        let n = TABLE_LEN;
        let mut data = vec![0.0; frames * LEVELS * STRIDE];
        let (mut spec_re, mut spec_im) = (vec![0.0; n], vec![0.0; n]);
        let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
        for f in 0..frames {
            for (k, x) in spec_re.iter_mut().enumerate() {
                *x = shape(f, k as f64 / n as f64);
            }
            spec_im.fill(0.0);
            fft(&mut spec_re, &mut spec_im, false);
            let mut scale = 1.0;
            for level in 0..LEVELS {
                let top = LEVEL0_HZ * (1 << level) as f32;
                let harmonics = ((sample_rate * 0.5 / top) as usize).clamp(1, n / 2 - 1);
                re.fill(0.0);
                im.fill(0.0);
                for h in 1..=harmonics {
                    re[h] = spec_re[h];
                    im[h] = spec_im[h];
                    re[n - h] = spec_re[n - h];
                    im[n - h] = spec_im[n - h];
                }
                fft(&mut re, &mut im, true);
                if level == 0 {
                    // Même gain pour tous les niveaux : pas de saut de volume d'une octave à l'autre.
                    let peak = re.iter().fold(0.0f64, |m, x| m.max(x.abs()));
                    scale = if peak > 1e-9 { 1.0 / peak } else { 1.0 };
                }
                let t = &mut data[(f * LEVELS + level) * STRIDE..][..STRIDE];
                for (dst, src) in t.iter_mut().zip(&re) {
                    *dst = (src * scale) as f32;
                }
                t[n] = t[0];
            }
        }
        Self { frames, data }
    }

    #[inline]
    fn table(&self, frame: usize, level: usize) -> &[f32] {
        &self.data[(frame * LEVELS + level) * STRIDE..][..STRIDE]
    }
}

/// Sinus → triangle → scie → carré → impulsion 25 %.
fn basic_shape(frame: usize, t: f64) -> f64 {
    match frame {
        0 => (TAU * t).sin(),
        1 => 1.0 - 4.0 * ((t + 0.25).fract() - 0.5).abs(),
        2 => 2.0 * (t + 0.5).fract() - 1.0,
        3 => {
            if t < 0.5 { 1.0 } else { -1.0 }
        }
        _ => {
            if t < 0.25 { 1.0 } else { -1.0 }
        }
    }
}

/// Spectres additifs de plus en plus riches : sinus, octave, orgue, impairs, buzz, formants.
fn harmonic_shape(frame: usize, t: f64) -> f64 {
    let amp = |n: f64| -> f64 {
        match frame {
            0 => f64::from(n == 1.0),
            1 => [0.0, 1.0, 0.5].get(n as usize).copied().unwrap_or(0.0),
            2 => f64::from(n <= 4.0) / n,
            3 => f64::from(n <= 15.0 && n % 2.0 == 1.0) / n,
            4 => f64::from(n <= 8.0),
            5 => 0.5 * f64::from(n == 1.0) + (-((n - 5.0) / 2.0).powi(2)).exp(),
            6 => 0.3 * f64::from(n == 1.0) + (-((n - 11.0) / 3.0).powi(2)).exp(),
            _ => 1.0 / n.sqrt(),
        }
    };
    (1..=48).map(|n| amp(n as f64) * (TAU * n as f64 * t).sin()).sum()
}

const SET_FRAMES: [usize; 2] = [5, 8];

fn build_tables(sample_rate: f32) -> [TableSet; 2] {
    [
        TableSet::build(SET_FRAMES[0], basic_shape, sample_rate),
        TableSet::build(SET_FRAMES[1], harmonic_shape, sample_rate),
    ]
}

/// FFT complexe radix 2 en place ; l'inverse est normalisée.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = if inverse { TAU } else { -TAU } / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in start..start + len / 2 {
                let b = k + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[k] - tr;
                im[b] = im[k] - ti;
                re[k] += tr;
                im[k] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
    if inverse {
        let s = 1.0 / n as f64;
        re.iter_mut().chain(im.iter_mut()).for_each(|x| *x *= s);
    }
}

/// Niveau de table dont toutes les harmoniques restent sous Nyquist pour `hz`.
fn mip_level(hz: f32) -> usize {
    if hz <= LEVEL0_HZ { 0 } else { ((hz / LEVEL0_HZ).log2().ceil() as usize).min(LEVELS - 1) }
}

struct Voice {
    note: u8,
    velocity: f32,
    started_at: u64,
    glide: Glide,
    phases: [f32; MAX_UNISON],
    sub_phase: f32,
    level: usize,
    level_hz: f32,
    amp_env: Adsr,
    filter_env: Adsr,
    filter: [Svf; 2],
    tick: u32,
}

/// Ponzik Wave, polyphonique huit voix, unisson jusqu'à huit oscillateurs par voix.
pub struct WaveSynth {
    set: usize,
    position: f32,
    unison: usize,
    detune: f32,
    spread: f32,
    sub: f32,
    cutoff: f32,
    resonance: f32,
    env_amount: f32,
    keytrack: f32,
    amp_env: AdsrParams,
    filter_env: AdsrParams,
    glide: f32,
    gain: f32,
    sample_rate: f32,
    glide_coef: f32,
    tables: [TableSet; 2],
    uni_ratio: [f32; MAX_UNISON],
    uni_gain: [(f32, f32); MAX_UNISON],
    voices: [Voice; VOICES],
    rng: WhiteNoise,
    note_counter: u64,
    last_note: Option<u8>,
}

impl Default for WaveSynth {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveSynth {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let env = AdsrParams { attack: 0.005, decay: 0.3, sustain: 0.7, release: 0.25 };
        let mut s = Self {
            set: 0,
            position: 0.5,
            unison: 1,
            detune: 0.0,
            spread: 0.0,
            sub: 0.0,
            cutoff: 2_000.0,
            resonance: 0.9,
            env_amount: 0.0,
            keytrack: 0.0,
            amp_env: env,
            filter_env: env,
            glide: 0.0,
            gain: 1.0,
            sample_rate: sr,
            glide_coef: 0.0,
            tables: build_tables(sr),
            uni_ratio: [1.0; MAX_UNISON],
            uni_gain: [(1.0, 1.0); MAX_UNISON],
            voices: std::array::from_fn(|_| Voice {
                note: 0,
                velocity: 0.0,
                started_at: 0,
                glide: Glide::default(),
                phases: [0.0; MAX_UNISON],
                sub_phase: 0.0,
                level: 0,
                level_hz: -1.0,
                amp_env: Adsr::new(env, sr),
                filter_env: Adsr::new(env, sr),
                filter: std::array::from_fn(|_| Svf::new(FilterMode::LowPass, 2_000.0, 0.9, sr)),
                tick: 0,
            }),
            rng: WhiteNoise::new(0x5EED_CAFE),
            note_counter: 0,
            last_note: None,
        };
        for p in &PARAMS {
            s.set_param(p.id, p.default);
        }
        s
    }

    fn update_unison(&mut self) {
        let n = self.unison;
        let norm = 1.0 / (n as f32).sqrt();
        for k in 0..MAX_UNISON {
            let o = if n > 1 { 2.0 * k as f32 / (n - 1) as f32 - 1.0 } else { 0.0 };
            self.uni_ratio[k] = 2f32.powf(o * self.detune / 1_200.0);
            let (gl, gr) = balance_gains(o * self.spread);
            self.uni_gain[k] = (gl * norm, gr * norm);
        }
    }
}

impl Params for WaveSynth {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_SET => self.set as f32,
            P_POSITION => self.position,
            P_UNISON => (self.unison - 1) as f32,
            P_DETUNE => self.detune,
            P_SPREAD => self.spread,
            P_SUB => self.sub,
            P_CUTOFF => self.cutoff,
            P_RESO => self.resonance,
            P_FENV => self.env_amount,
            P_KEYTRACK => self.keytrack,
            P_ATTACK => self.amp_env.attack,
            P_DECAY => self.amp_env.decay,
            P_SUSTAIN => self.amp_env.sustain,
            P_RELEASE => self.amp_env.release,
            P_FATTACK => self.filter_env.attack,
            P_FDECAY => self.filter_env.decay,
            P_FSUSTAIN => self.filter_env.sustain,
            P_FRELEASE => self.filter_env.release,
            P_GLIDE => self.glide,
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_SET => self.set = v as usize,
            P_POSITION => self.position = v,
            P_UNISON => self.unison = v as usize + 1,
            P_DETUNE => self.detune = v,
            P_SPREAD => self.spread = v,
            P_SUB => self.sub = v,
            P_CUTOFF => self.cutoff = v,
            P_RESO => self.resonance = v,
            P_FENV => self.env_amount = v,
            P_KEYTRACK => self.keytrack = v,
            P_ATTACK => self.amp_env.attack = v,
            P_DECAY => self.amp_env.decay = v,
            P_SUSTAIN => self.amp_env.sustain = v,
            P_RELEASE => self.amp_env.release = v,
            P_FATTACK => self.filter_env.attack = v,
            P_FDECAY => self.filter_env.decay = v,
            P_FSUSTAIN => self.filter_env.sustain = v,
            P_FRELEASE => self.filter_env.release = v,
            P_GLIDE => {
                self.glide = v;
                self.glide_coef = glide_coef(v, self.sample_rate);
            }
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        if matches!(id, P_UNISON | P_DETUNE | P_SPREAD) {
            self.update_unison();
        }
        let (amp, filt) = (self.amp_env, self.filter_env);
        for voice in &mut self.voices {
            voice.amp_env.params = amp;
            voice.filter_env.params = filt;
        }
    }
}

impl Instrument for WaveSynth {
    fn kind(&self) -> &'static str {
        "wavetable"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate != self.sample_rate {
            self.tables = build_tables(sample_rate);
        }
        self.sample_rate = sample_rate;
        self.glide_coef = glide_coef(self.glide, sample_rate);
        for v in &mut self.voices {
            v.amp_env.set_sample_rate(sample_rate);
            v.filter_env.set_sample_rate(sample_rate);
            v.level_hz = -1.0;
        }
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let idx = pick_voice(VOICES, |i| self.voices[i].amp_env.is_active(), |i| self.voices[i].started_at);
        self.note_counter += 1;
        let from = self.last_note.filter(|_| self.glide > 0.0);
        let v = &mut self.voices[idx];
        v.note = note;
        v.velocity = velocity.clamp(0.0, 1.0);
        v.started_at = self.note_counter;
        match from {
            Some(f) => {
                v.glide.jump(f as f32);
                v.glide.go(note as f32);
            }
            None => v.glide.jump(note as f32),
        }
        if !v.amp_env.is_active() {
            for p in &mut v.phases {
                *p = (self.rng.next_sample() * 0.5 + 0.5).fract();
            }
            v.sub_phase = 0.0;
            v.tick = 0;
            v.filter.iter_mut().for_each(Svf::reset);
        }
        v.level_hz = -1.0;
        v.amp_env.gate_on();
        v.filter_env.gate_on();
        self.last_note = Some(note);
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.amp_env.is_gated()) {
            v.amp_env.gate_off();
            v.filter_env.gate_off();
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.amp_env.reset();
            v.filter_env.reset();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let inv_sr = 1.0 / sr;
        let set = &self.tables[self.set];
        let fpos = self.position * (set.frames - 1) as f32;
        let fa = (fpos as usize).min(set.frames - 1);
        let fb = (fa + 1).min(set.frames - 1);
        let fr = fpos - fa as f32;
        let (n_uni, ratio, ugain) = (self.unison, self.uni_ratio, self.uni_gain);
        let (sub, cutoff, reso, env_amt, key) = (self.sub, self.cutoff, self.resonance, self.env_amount, self.keytrack);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut ml, mut mr) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.amp_env.is_active()) {
                let hz = v.glide.next(self.glide_coef);
                if hz != v.level_hz {
                    v.level_hz = hz;
                    v.level = mip_level(hz * MAX_DETUNE_RATIO);
                }
                let (ta, tb) = (set.table(fa, v.level), set.table(fb, v.level));
                let (mut sl, mut sr) = (0.0, 0.0);
                for k in 0..n_uni {
                    let p = v.phases[k];
                    let x = p * TABLE_LEN as f32;
                    let i = (x as usize).min(TABLE_LEN - 1);
                    let t = x - i as f32;
                    let a = ta[i] + (ta[i + 1] - ta[i]) * t;
                    let b = tb[i] + (tb[i + 1] - tb[i]) * t;
                    let s = a + (b - a) * fr;
                    sl += s * ugain[k].0;
                    sr += s * ugain[k].1;
                    v.phases[k] = (p + hz * ratio[k] * inv_sr).fract();
                }
                let s = fast_sin(v.sub_phase) * sub;
                v.sub_phase = (v.sub_phase + 0.5 * hz * inv_sr).fract();
                let fenv = v.filter_env.next_sample();
                if v.tick == 0 {
                    let st = env_amt * fenv + key * (v.glide.note() - 60.0);
                    let fc = cutoff * 2f32.powf(st / 12.0);
                    for f in &mut v.filter {
                        f.set_params(fc, reso, self.sample_rate);
                    }
                }
                v.tick = (v.tick + 1) % CONTROL_RATE;
                let amp = v.amp_env.next_sample() * v.velocity;
                ml += v.filter[0].process(sl + s) * amp;
                mr += v.filter[1].process(sr + s) * amp;
            }
            *l += soft_clip(ml * self.gain);
            *r += soft_clip(mr * self.gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, rising_crossings, stress};
    use super::*;

    #[test]
    fn plays_and_releases() {
        let mut w = WaveSynth::new();
        assert_eq!(peak(&mut w, 256), 0.0);
        w.note_on(48, 1.0);
        assert!(peak(&mut w, 4_800) > 0.05);
        w.note_off(48);
        peak(&mut w, 48_000);
        assert_eq!(peak(&mut w, 512), 0.0);
    }

    #[test]
    fn sine_frame_has_the_right_pitch() {
        let mut w = WaveSynth::new();
        w.set_param(P_POSITION, 0.0);
        w.set_param(P_UNISON, 0.0);
        w.set_param(P_CUTOFF, 20_000.0);
        w.set_param(P_FENV, 0.0);
        w.note_on(69, 1.0);
        render(&mut w, 4_800);
        let (l, _) = render(&mut w, 48_000);
        let n = rising_crossings(&l);
        assert!((439..=441).contains(&n), "{n}");
    }

    #[test]
    fn tables_are_band_limited() {
        let sr = 48_000.0;
        let set = TableSet::build(5, basic_shape, sr);
        let level = mip_level(5_000.0);
        let allowed = (sr * 0.5 / (LEVEL0_HZ * (1 << level) as f32)) as usize;
        assert!(5_000.0 * allowed as f32 <= sr * 0.5);
        let t = set.table(2, level);
        let mag = |h: usize| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (k, &x) in t[..TABLE_LEN].iter().enumerate() {
                let a = TAU * (h * k) as f64 / TABLE_LEN as f64;
                re += x as f64 * a.cos();
                im += x as f64 * a.sin();
            }
            (re * re + im * im).sqrt() / TABLE_LEN as f64
        };
        assert!(mag(1) > 0.1);
        for h in allowed + 1..allowed + 40 {
            assert!(mag(h) < 1e-5, "harmonique {h} : {}", mag(h));
        }
        assert!((t[TABLE_LEN] - t[0]).abs() < 1e-9);
    }

    #[test]
    fn unison_spreads_in_stereo() {
        let mut w = WaveSynth::new();
        w.set_param(P_UNISON, 7.0);
        w.set_param(P_SPREAD, 1.0);
        w.note_on(60, 1.0);
        let (l, r) = render(&mut w, 4_800);
        assert!(l.iter().zip(&r).any(|(a, b)| (a - b).abs() > 1e-3));
    }

    #[test]
    fn survives_extremes() {
        let mut w = WaveSynth::new();
        stress(&mut w);
    }
}
