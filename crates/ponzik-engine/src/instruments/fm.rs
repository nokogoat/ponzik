//! Ponzik FM : synthé FM à quatre opérateurs, façon Operator / Sytrus.
//!
//! Chaque opérateur est un sinus avec sa propre enveloppe ; l'algorithme
//! décide qui module qui. Les porteuses sont mélangées puis filtrées.

use ponzik_dsp::env::{Adsr, AdsrParams};
use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::util::{db_to_gain, gain_to_db};

use super::common::{Expr, Glide, Tune, fast_sin, glide_coef, pan_gains, pick_voice, soft_clip};
use crate::{Controller, Instrument, NoteExpression};
use crate::param::{ParamInfo, Params, Unit};

const VOICES: usize = 8;
const OPS: usize = 4;
/// Excursion de phase maximale d'un modulateur, en tours.
const MOD_DEPTH: f32 = 1.0;
const FB_DEPTH: f32 = 0.5;

const COARSE: &[&str] = &["0.5", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16"];
const ALGO_NAMES: &[&str] = &[
    "4 → 3 → 2 → 1",
    "(3 + 4) → 2 → 1",
    "(4 → 3) + 2 → 1",
    "2 → 1, 4 → 3",
    "4 → (1, 2, 3)",
    "4 → 3 → 2, 1",
    "(2 + 3 + 4) → 1",
    "parallel",
];
const FILTERS: &[&str] = &["Low-pass", "High-pass"];
const OP_GROUPS: [&str; OPS] = ["Operator 1", "Operator 2", "Operator 3", "Operator 4"];

/// `mods[i]` : masque des opérateurs qui modulent `i` (bit j = opérateur j + 1).
/// Un modulateur a toujours un indice supérieur à sa cible : on calcule de 4 vers 1.
struct Algo {
    mods: [u8; OPS],
    carriers: u8,
}

const ALGOS: [Algo; 8] = [
    Algo { mods: [0b0010, 0b0100, 0b1000, 0], carriers: 0b0001 },
    Algo { mods: [0b0010, 0b1100, 0, 0], carriers: 0b0001 },
    Algo { mods: [0b0110, 0, 0b1000, 0], carriers: 0b0001 },
    Algo { mods: [0b0010, 0, 0b1000, 0], carriers: 0b0101 },
    Algo { mods: [0b1000, 0b1000, 0b1000, 0], carriers: 0b0111 },
    Algo { mods: [0, 0b0100, 0b1000, 0], carriers: 0b0011 },
    Algo { mods: [0b1110, 0, 0, 0], carriers: 0b0001 },
    Algo { mods: [0; OPS], carriers: 0b1111 },
];

// Paramètres d'opérateur : id = 10 × (opérateur − 1) + champ.
const F_COARSE: u32 = 0;
const F_FINE: u32 = 1;
const F_LEVEL: u32 = 2;
const F_ATTACK: u32 = 3;
const F_DECAY: u32 = 4;
const F_SUSTAIN: u32 = 5;
const F_RELEASE: u32 = 6;
const F_VEL: u32 = 7;
const OP_FIELDS: usize = 8;

const P_ALGO: u32 = 40;
const P_FEEDBACK: u32 = 41;
const P_FILTER: u32 = 42;
const P_CUTOFF: u32 = 43;
const P_RESO: u32 = 44;
const P_GLIDE: u32 = 45;
const P_VOLUME: u32 = 46;

const N_PARAMS: usize = OPS * OP_FIELDS + 7;
static PARAMS: [ParamInfo; N_PARAMS] = build_params();

const fn build_params() -> [ParamInfo; N_PARAMS] {
    let mut p = [ParamInfo::new(0, "", 0.0, 0.0, 0.0, Unit::None); N_PARAMS];
    let mut op = 0;
    while op < OPS {
        let g = OP_GROUPS[op];
        let b = op as u32 * 10;
        let k = op * OP_FIELDS;
        // Par défaut : 1 porteuse, 2 module avec un déclin court, 3 et 4 muets.
        let (level, decay, sustain) = match op {
            0 => (1.0, 1.2, 0.7),
            1 => (0.45, 0.6, 0.25),
            _ => (0.0, 0.5, 0.5),
        };
        p[k] = ParamInfo::choice(b + F_COARSE, "Coarse", COARSE, 1).group(g);
        p[k + 1] = ParamInfo::new(b + F_FINE, "Fine", 0.0, 0.99, 0.0, Unit::None).group(g);
        p[k + 2] = ParamInfo::new(b + F_LEVEL, "Level", 0.0, 1.0, level, Unit::Percent).group(g);
        p[k + 3] = ParamInfo::new(b + F_ATTACK, "Attack", 0.001, 4.0, 0.002, Unit::Seconds).log().group(g);
        p[k + 4] = ParamInfo::new(b + F_DECAY, "Decay", 0.005, 8.0, decay, Unit::Seconds).log().group(g);
        p[k + 5] = ParamInfo::new(b + F_SUSTAIN, "Sustain", 0.0, 1.0, sustain, Unit::Percent).group(g);
        p[k + 6] = ParamInfo::new(b + F_RELEASE, "Release", 0.005, 8.0, 0.3, Unit::Seconds).log().group(g);
        p[k + 7] = ParamInfo::new(b + F_VEL, "Velocity", 0.0, 1.0, 0.5, Unit::Percent).group(g);
        op += 1;
    }
    let k = OPS * OP_FIELDS;
    p[k] = ParamInfo::choice(P_ALGO, "Algorithm", ALGO_NAMES, 0).group("Algorithm");
    p[k + 1] = ParamInfo::new(P_FEEDBACK, "Feedback 4", 0.0, 1.0, 0.0, Unit::Percent).group("Algorithm");
    p[k + 2] = ParamInfo::choice(P_FILTER, "Type", FILTERS, 0).group("Filter");
    p[k + 3] = ParamInfo::new(P_CUTOFF, "Cutoff", 30.0, 20_000.0, 20_000.0, Unit::Hz).log().group("Filter");
    p[k + 4] = ParamInfo::new(P_RESO, "Resonance", 0.5, 10.0, 0.707, Unit::None).log().group("Filter");
    p[k + 5] = ParamInfo::new(P_GLIDE, "Portamento", 0.0, 2.0, 0.0, Unit::Seconds).group("Global");
    p[k + 6] = ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -10.0, Unit::Db).group("Global");
    p
}

#[derive(Clone, Copy, Debug)]
struct Operator {
    coarse: usize,
    fine: f32,
    level: f32,
    env: AdsrParams,
    velocity: f32,
}

impl Operator {
    fn ratio(&self) -> f32 {
        if self.coarse == 0 { 0.5 + self.fine } else { self.coarse as f32 + self.fine }
    }
}

struct Voice {
    note: u8,
    started_at: u64,
    glide: Glide,
    phase: [f32; OPS],
    env: [Adsr; OPS],
    amp: [f32; OPS],
    /// Deux dernières sorties de l'opérateur 4 (rétroaction moyennée, façon DX7).
    fb: [f32; 2],
    expr: Expr,
    tune: Tune,
}

impl Voice {
    fn sounding(&self, carriers: u8) -> bool {
        (0..OPS).any(|i| carriers & (1 << i) != 0 && self.env[i].is_active())
    }
}

/// Ponzik FM, polyphonique huit voix.
pub struct FmSynth {
    ops: [Operator; OPS],
    algo: usize,
    feedback: f32,
    filter_mode: usize,
    cutoff: f32,
    resonance: f32,
    glide: f32,
    gain: f32,
    sample_rate: f32,
    glide_coef: f32,
    filter: Svf,
    /// Right side: voices can be panned.
    filter_r: Svf,
    voices: [Voice; VOICES],
    note_counter: u64,
    last_note: Option<u8>,
    /// Pitch bend in semitones.
    bend: f32,
}

impl Default for FmSynth {
    fn default() -> Self {
        Self::new()
    }
}

impl FmSynth {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let env = AdsrParams { attack: 0.002, decay: 0.5, sustain: 0.5, release: 0.3 };
        let op = Operator { coarse: 1, fine: 0.0, level: 0.0, env, velocity: 0.5 };
        let mut s = Self {
            ops: [op; OPS],
            algo: 0,
            feedback: 0.0,
            filter_mode: 0,
            cutoff: 20_000.0,
            resonance: 0.707,
            glide: 0.0,
            gain: 1.0,
            sample_rate: sr,
            glide_coef: 0.0,
            filter: Svf::new(FilterMode::LowPass, 20_000.0, 0.707, sr),
            filter_r: Svf::new(FilterMode::LowPass, 20_000.0, 0.707, sr),
            voices: std::array::from_fn(|_| Voice {
                note: 0,
                started_at: 0,
                glide: Glide::default(),
                phase: [0.0; OPS],
                env: std::array::from_fn(|_| Adsr::new(env, sr)),
                amp: [0.0; OPS],
                fb: [0.0; 2],
                expr: Expr::default(),
                tune: Tune::default(),
            }),
            note_counter: 0,
            last_note: None,
            bend: 0.0,
        };
        for p in &PARAMS {
            s.set_param(p.id, p.default);
        }
        s
    }

    fn update_filter(&mut self) {
        self.filter.mode = if self.filter_mode == 0 { FilterMode::LowPass } else { FilterMode::HighPass };
        self.filter.set_params(self.cutoff, self.resonance, self.sample_rate);
        self.filter_r.mode = self.filter.mode;
        self.filter_r.set_params(self.cutoff, self.resonance, self.sample_rate);
    }
}

impl Params for FmSynth {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        if id < 40 {
            let Some(op) = self.ops.get(id as usize / 10) else { return 0.0 };
            return match id % 10 {
                F_COARSE => op.coarse as f32,
                F_FINE => op.fine,
                F_LEVEL => op.level,
                F_ATTACK => op.env.attack,
                F_DECAY => op.env.decay,
                F_SUSTAIN => op.env.sustain,
                F_RELEASE => op.env.release,
                F_VEL => op.velocity,
                _ => 0.0,
            };
        }
        match id {
            P_ALGO => self.algo as f32,
            P_FEEDBACK => self.feedback,
            P_FILTER => self.filter_mode as f32,
            P_CUTOFF => self.cutoff,
            P_RESO => self.resonance,
            P_GLIDE => self.glide,
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        if id < 40 {
            let i = id as usize / 10;
            let op = &mut self.ops[i];
            match id % 10 {
                F_COARSE => op.coarse = v as usize,
                F_FINE => op.fine = v,
                F_LEVEL => op.level = v,
                F_ATTACK => op.env.attack = v,
                F_DECAY => op.env.decay = v,
                F_SUSTAIN => op.env.sustain = v,
                F_RELEASE => op.env.release = v,
                F_VEL => op.velocity = v,
                _ => {}
            }
            let env = op.env;
            for voice in &mut self.voices {
                voice.env[i].params = env;
            }
            return;
        }
        match id {
            P_ALGO => self.algo = v as usize,
            P_FEEDBACK => self.feedback = v,
            P_FILTER => self.filter_mode = v as usize,
            P_CUTOFF => self.cutoff = v,
            P_RESO => self.resonance = v,
            P_GLIDE => {
                self.glide = v;
                self.glide_coef = glide_coef(v, self.sample_rate);
            }
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        if matches!(id, P_FILTER | P_CUTOFF | P_RESO) {
            self.update_filter();
        }
    }
}

impl Instrument for FmSynth {
    fn kind(&self) -> &'static str {
        "fm"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.glide_coef = glide_coef(self.glide, sample_rate);
        for v in &mut self.voices {
            for e in &mut v.env {
                e.set_sample_rate(sample_rate);
            }
        }
        self.update_filter();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let carriers = ALGOS[self.algo].carriers;
        let idx = pick_voice(VOICES, |i| self.voices[i].sounding(carriers), |i| self.voices[i].started_at);
        self.note_counter += 1;
        let velocity = velocity.clamp(0.0, 1.0);
        let from = self.last_note.filter(|_| self.glide > 0.0);
        let v = &mut self.voices[idx];
        let was_sounding = v.sounding(carriers);
        v.note = note;
        v.started_at = self.note_counter;
        v.expr.reset();
        match from {
            Some(f) => {
                v.glide.jump(f as f32);
                v.glide.go(note as f32);
            }
            None => v.glide.jump(note as f32),
        }
        for (i, op) in self.ops.iter().enumerate() {
            v.amp[i] = op.level * (1.0 - op.velocity + op.velocity * velocity);
            if !was_sounding {
                v.phase[i] = 0.0;
            }
            v.env[i].gate_on();
        }
        if !was_sounding {
            v.fb = [0.0; 2];
        }
        self.last_note = Some(note);
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note) {
            for e in v.env.iter_mut().filter(|e| e.is_gated()) {
                e.gate_off();
            }
        }
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        let sr = self.sample_rate;
        let carriers = ALGOS[self.algo].carriers;
        let gated = |v: &Voice| (0..OPS).any(|i| carriers & (1 << i) != 0 && v.env[i].is_gated());
        if let Some(v) = self.voices.iter_mut().filter(|v| v.note == key && gated(v)).max_by_key(|v| v.started_at) {
            v.expr.apply(expr, sr);
        }
    }

    fn controller(&mut self, ctl: Controller) {
        if let Controller::PitchBend(b) = ctl {
            self.bend = b.clamp(-1.0, 1.0) * crate::PITCH_BEND_RANGE;
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            for e in &mut v.env {
                e.reset();
            }
        }
        self.filter.reset();
        self.filter_r.reset();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let algo = &ALGOS[self.algo];
        let carriers = algo.carriers;
        if !self.voices.iter().any(|v| v.sounding(carriers)) {
            self.filter.reset();
            self.filter_r.reset();
            return;
        }
        let norm = self.gain / carriers.count_ones() as f32;
        let ratios: [f32; OPS] = std::array::from_fn(|i| self.ops[i].ratio() / self.sample_rate);
        let fb = self.feedback * FB_DEPTH;
        let bend = self.bend;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut ml, mut mr) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.sounding(carriers)) {
                let hz = v.glide.next(self.glide_coef) * v.tune.ratio(v.expr.tuning.tick() + bend);
                let mut mix = 0.0;
                let mut out = [0.0f32; OPS];
                for i in (0..OPS).rev() {
                    let mut m = 0.0;
                    for (j, o) in out.iter().enumerate().skip(i + 1) {
                        if algo.mods[i] & (1 << j) != 0 {
                            m += o;
                        }
                    }
                    if i == OPS - 1 {
                        m += fb * (v.fb[0] + v.fb[1]);
                    }
                    let env = v.env[i].next_sample();
                    out[i] = fast_sin(v.phase[i] + m * MOD_DEPTH) * env * v.amp[i];
                    v.phase[i] = (v.phase[i] + hz * ratios[i]).fract();
                }
                v.fb = [out[OPS - 1], v.fb[0]];
                for (i, o) in out.iter().enumerate() {
                    if carriers & (1 << i) != 0 {
                        mix += o;
                    }
                }
                let (pl, pr) = pan_gains(v.expr.pan);
                ml += mix * pl;
                mr += mix * pr;
            }
            *l += soft_clip(self.filter.process(ml * norm));
            *r += soft_clip(self.filter_r.process(mr * norm));
        }
        // Porteuses éteintes : on coupe aussi les modulateurs.
        for v in self.voices.iter_mut().filter(|v| !v.sounding(carriers)) {
            for e in &mut v.env {
                e.reset();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, rising_crossings, stress};
    use super::*;

    #[test]
    fn plays_and_releases() {
        let mut fm = FmSynth::new();
        fm.set_sample_rate(48_000.0);
        assert_eq!(peak(&mut fm, 256), 0.0);
        fm.note_on(57, 1.0);
        assert!(peak(&mut fm, 4_800) > 0.05);
        fm.note_off(57);
        peak(&mut fm, 48_000);
        assert_eq!(peak(&mut fm, 512), 0.0);
    }

    #[test]
    fn carrier_alone_has_the_right_pitch() {
        let mut fm = FmSynth::new();
        fm.set_sample_rate(48_000.0);
        fm.set_param(F_LEVEL + 10, 0.0);
        fm.note_on(69, 1.0);
        render(&mut fm, 4_800);
        let (l, _) = render(&mut fm, 48_000);
        let n = rising_crossings(&l);
        assert!((439..=441).contains(&n), "{n}");
        fm.set_param(F_COARSE, 3.0);
        let (l, _) = render(&mut fm, 48_000);
        let n = rising_crossings(&l);
        assert!((1_319..=1_321).contains(&n), "{n}");
    }

    #[test]
    fn modulation_adds_harmonics() {
        let mut fm = FmSynth::new();
        fm.set_param(F_LEVEL + 10, 1.0);
        fm.set_param(F_SUSTAIN + 10, 1.0);
        fm.note_on(45, 1.0);
        let (l, _) = render(&mut fm, 48_000);
        // Un sinus pur à 110 Hz croiserait zéro 110 fois.
        assert!(rising_crossings(&l) > 150);
    }

    #[test]
    fn steals_voices_and_survives_extremes() {
        let mut fm = FmSynth::new();
        for n in 0..(VOICES as u8 * 2) {
            fm.note_on(40 + n, 0.8);
        }
        assert!(fm.voices.iter().all(|v| v.note >= 40 + VOICES as u8));
        for algo in 0..ALGOS.len() {
            fm.set_param(P_ALGO, algo as f32);
            fm.set_param(P_FEEDBACK, 1.0);
            assert!(peak(&mut fm, 1_024).is_finite());
        }
        stress(&mut fm);
    }
}
