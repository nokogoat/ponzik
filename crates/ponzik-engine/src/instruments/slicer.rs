//! Découpeur : découpe un échantillon en tranches jouées chacune par une note.
//!
//! La tranche k est jouée par la note 60 + k (Do4 et au-dessus) ; les notes
//! plus graves ou au-delà de la dernière tranche sont muettes. Découpe en
//! parts égales ou sur les transitoires : l'énergie par bloc est calculée au
//! chargement, la recherche des attaques (rapide, sans allocation) est refaite
//! à chaque changement de mode, de nombre de tranches ou de sensibilité. En
//! mode « Marqueurs », les tranches suivent les marqueurs de l'échantillon
//! (posés dans l'Atelier, lus dans le bloc `cue ` du WAV), 64 au plus.
//!
//! `is_melodic` vaut vrai : l'éditeur affiche alors les noms de notes, ce qui
//! permet de choisir la tranche à la saisie (la note ne change pas la hauteur).

use std::sync::Arc;

use ponzik_dsp::env::{Adsr, AdsrParams};
use ponzik_dsp::util::{db_to_gain, gain_to_db};

use super::common::{pick_voice, read_cubic, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};
use crate::sample::Sample;

const VOICES: usize = 8;
const MAX_SLICES: usize = 64;
const FIRST_NOTE: u8 = 60;
/// Taille des blocs d'analyse des transitoires, en images.
const HOP: usize = 256;
/// Plancher d'analyse sous le bloc le plus fort, en dB.
const FLOOR_DB: f32 = 60.0;
/// Écart minimal entre deux attaques, en secondes.
const MIN_GAP: f32 = 0.03;

const MODES: &[&str] = &["Equal Slices", "Transients", "Markers"];
const COUNTS: &[&str] = &["4", "8", "16", "32"];
const COUNT_VALUES: [usize; 4] = [4, 8, 16, 32];
const OFF_ON: &[&str] = &["No", "Yes"];

const P_MODE: u32 = 0;
const P_SLICES: u32 = 1;
const P_SENSITIVITY: u32 = 2;
const P_ATTACK: u32 = 3;
const P_RELEASE: u32 = 4;
const P_PITCH: u32 = 5;
const P_REVERSE: u32 = 6;
const P_VOLUME: u32 = 7;

static PARAMS: [ParamInfo; 8] = [
    ParamInfo::choice(P_MODE, "Mode", MODES, 0).group("Slicing"),
    ParamInfo::choice(P_SLICES, "Slices", COUNTS, 1).group("Slicing"),
    ParamInfo::new(P_SENSITIVITY, "Sensitivity", 0.0, 1.0, 0.5, Unit::Percent).group("Slicing"),
    ParamInfo::new(P_ATTACK, "Attack", 0.0005, 1.0, 0.001, Unit::Seconds).log().group("Slice"),
    ParamInfo::new(P_RELEASE, "Release", 0.001, 4.0, 0.02, Unit::Seconds).log().group("Slice"),
    ParamInfo::new(P_PITCH, "Transpose", -24.0, 24.0, 0.0, Unit::Semitones).group("Slice"),
    ParamInfo::choice(P_REVERSE, "Reverse", OFF_ON, 0).group("Slice"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -3.0, Unit::Db).group("Output"),
];

struct Voice {
    note: u8,
    velocity: f32,
    started_at: u64,
    playing: bool,
    /// Bornes de la tranche en images de l'échantillon, figées au déclenchement.
    start: usize,
    end: usize,
    /// Position dans le sens de lecture, en images depuis le début de la tranche.
    pos: f64,
    env: Adsr,
}

/// Découpeur polyphonique huit voix ; muet tant qu'aucun échantillon n'est chargé.
pub struct Slicer {
    sample: Option<Arc<Sample>>,
    /// Énergie de chaque bloc de `HOP` images, en dB.
    energy: Vec<f32>,
    /// Début des tranches ; `bounds[n_slices]` est la fin de l'échantillon.
    bounds: [usize; MAX_SLICES + 1],
    n_slices: usize,
    /// 0 : parts égales, 1 : transitoires, 2 : marqueurs de l'échantillon.
    mode: usize,
    count: usize,
    sensitivity: f32,
    env: AdsrParams,
    pitch: f32,
    reverse: bool,
    gain: f32,
    sample_rate: f32,
    voices: [Voice; VOICES],
    note_counter: u64,
}

impl Default for Slicer {
    fn default() -> Self {
        Self::new()
    }
}

impl Slicer {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let env = AdsrParams::new(0.001, 0.001, 1.0, 0.02);
        let mut s = Self {
            sample: None,
            energy: Vec::new(),
            bounds: [0; MAX_SLICES + 1],
            n_slices: 0,
            mode: 0,
            count: 1,
            sensitivity: 0.5,
            env,
            pitch: 0.0,
            reverse: false,
            gain: 1.0,
            sample_rate: sr,
            voices: std::array::from_fn(|_| Voice {
                note: 0,
                velocity: 0.0,
                started_at: 0,
                playing: false,
                start: 0,
                end: 0,
                pos: 0.0,
                env: Adsr::new(env, sr),
            }),
            note_counter: 0,
        };
        for p in &PARAMS {
            s.set_param(p.id, p.default);
        }
        s
    }

    /// Bornes des tranches courantes (début de chacune, puis fin de l'échantillon).
    pub fn slice_bounds(&self) -> &[usize] {
        &self.bounds[..self.n_slices + 1]
    }

    /// Recalcule les tranches ; sans allocation, appelable depuis le thread audio.
    fn compute_bounds(&mut self) {
        let Some(sample) = self.sample.as_deref() else {
            self.n_slices = 0;
            return;
        };
        let len = sample.len();
        let n = COUNT_VALUES[self.count];
        self.bounds = [len; MAX_SLICES + 1];
        self.bounds[0] = 0;
        if len < 2 {
            self.n_slices = 0;
            return;
        }
        if self.mode == 2 && !sample.markers.is_empty() {
            // Marqueurs posés dans l'Atelier (bloc « cue » du WAV).
            let mut count = 1;
            for &m in &sample.markers {
                if count < MAX_SLICES && m > self.bounds[count - 1] && m < len - 1 {
                    self.bounds[count] = m;
                    count += 1;
                }
            }
            self.bounds[count] = len;
            self.n_slices = count;
            return;
        }
        if self.mode != 1 || self.energy.len() < 3 {
            for k in 0..=n {
                self.bounds[k] = (len as u64 * k as u64 / n as u64) as usize;
            }
            self.n_slices = n;
            return;
        }
        // Montée d'énergie d'un bloc à l'autre, en dB, au-dessus d'un plancher.
        let max = self.energy.iter().fold(f32::MIN, |m, &e| m.max(e));
        let floor = max - FLOOR_DB;
        let level = |k: usize| self.energy[k].max(floor);
        let rise = |k: usize| level(k) - level(k - 1);
        let threshold = 3.0 + 21.0 * (1.0 - self.sensitivity);
        let gap = ((MIN_GAP * sample.sample_rate) as usize / HOP).max(1);
        // Les `n − 1` attaques les plus franches : (bloc, montée).
        let mut found = [(0usize, 0.0f32); MAX_SLICES];
        let mut used = 0;
        let mut last = 0;
        let blocks = self.energy.len();
        for k in 1..blocks {
            let r = rise(k);
            let is_peak = r > threshold && (k + 1 >= blocks || r >= rise(k + 1)) && k - last >= gap;
            if !is_peak {
                continue;
            }
            last = k;
            if used < n - 1 {
                found[used] = (k, r);
                used += 1;
            } else if let Some(weak) = (0..used).min_by(|&a, &b| found[a].1.total_cmp(&found[b].1))
                && found[weak].1 < r
            {
                found[weak] = (k, r);
            }
        }
        let found = &mut found[..used];
        found.sort_unstable_by_key(|f| f.0);
        let mono = |i: usize| 0.5 * (sample.left[i] + sample.right[i]);
        let mut count = 1;
        for &(k, _) in found.iter() {
            // Affinage : premier échantillon franchissant 10 % de la crête du bloc.
            let lo = (k - 1) * HOP;
            let hi = ((k + 1) * HOP).min(len);
            let crest = (k * HOP..hi).fold(0.0f32, |m, i| m.max(mono(i).abs()));
            let at = (lo..hi).find(|&i| mono(i).abs() > 0.1 * crest).unwrap_or(k * HOP);
            if at > self.bounds[count - 1] && at < len - 1 {
                self.bounds[count] = at;
                count += 1;
            }
        }
        self.bounds[count] = len;
        self.n_slices = count;
    }
}

impl Params for Slicer {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        let b = |x: bool| f32::from(u8::from(x));
        match id {
            P_MODE => self.mode as f32,
            P_SLICES => self.count as f32,
            P_SENSITIVITY => self.sensitivity,
            P_ATTACK => self.env.attack,
            P_RELEASE => self.env.release,
            P_PITCH => self.pitch,
            P_REVERSE => b(self.reverse),
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_MODE => self.mode = v.round() as usize,
            P_SLICES => self.count = v as usize,
            P_SENSITIVITY => self.sensitivity = v,
            P_ATTACK => self.env.attack = v,
            P_RELEASE => self.env.release = v,
            P_PITCH => self.pitch = v,
            P_REVERSE => self.reverse = v > 0.5,
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        match id {
            P_MODE | P_SLICES | P_SENSITIVITY => self.compute_bounds(),
            P_ATTACK | P_RELEASE => {
                let env = self.env;
                for voice in &mut self.voices {
                    voice.env.params = env;
                }
            }
            _ => {}
        }
    }
}

impl Instrument for Slicer {
    fn kind(&self) -> &'static str {
        "slicer"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for v in &mut self.voices {
            v.env.set_sample_rate(sample_rate);
        }
    }

    fn set_sample(&mut self, sample: Arc<Sample>) {
        self.all_notes_off();
        self.energy = sample
            .left
            .chunks(HOP)
            .zip(sample.right.chunks(HOP))
            .map(|(l, r)| {
                let sum: f32 = l.iter().zip(r).map(|(a, b)| (0.5 * (a + b)).powi(2)).sum();
                10.0 * (sum / l.len() as f32 + 1e-12).log10()
            })
            .collect();
        self.sample = Some(sample);
        self.compute_bounds();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        if self.sample.is_none() || note < FIRST_NOTE {
            return;
        }
        let k = (note - FIRST_NOTE) as usize;
        if k >= self.n_slices || self.bounds[k + 1] < self.bounds[k] + 2 {
            return;
        }
        let idx = pick_voice(VOICES, |i| self.voices[i].playing, |i| self.voices[i].started_at);
        self.note_counter += 1;
        let v = &mut self.voices[idx];
        v.note = note;
        v.velocity = velocity.clamp(0.0, 1.0);
        v.started_at = self.note_counter;
        v.playing = true;
        v.start = self.bounds[k];
        v.end = self.bounds[k + 1];
        v.pos = 0.0;
        v.env.gate_on();
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.playing && v.note == note && v.env.is_gated()) {
            v.env.gate_off();
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.playing = false;
            v.env.reset();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let Some(sample) = self.sample.as_deref() else { return };
        if sample.is_empty() || self.voices.iter().all(|v| !v.playing) {
            return;
        }
        let rate = sample.sample_rate as f64 / self.sample_rate as f64 * (self.pitch as f64 / 12.0).exp2();
        let release = (self.env.release * self.sample_rate) as f64;
        let reverse = self.reverse;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut ml, mut mr) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.playing) {
                let span = (v.end - v.start) as f64;
                // La relâche se termine pile en fin de tranche : pas de clic.
                if v.env.is_gated() && (span - v.pos) / rate <= release {
                    v.env.gate_off();
                }
                let at = if reverse { v.end as f64 - 1.0 - v.pos } else { v.start as f64 + v.pos };
                let amp = v.env.next_sample() * v.velocity;
                ml += read_cubic(&sample.left, at) * amp;
                mr += read_cubic(&sample.right, at) * amp;
                v.pos += rate;
                if v.pos >= span || !v.env.is_active() {
                    v.playing = false;
                    v.env.reset();
                }
            }
            *l += soft_clip(ml * self.gain);
            *r += soft_clip(mr * self.gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, stress};
    use super::*;
    use ponzik_dsp::noise::WhiteNoise;

    /// Huit paliers de 1 000 images : la tranche k vaut 0,1 × (k + 1).
    fn steps() -> Arc<Sample> {
        let left: Vec<f32> = (0..8_000).map(|i| 0.1 * (i / 1_000 + 1) as f32).collect();
        Arc::new(Sample { name: "paliers".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() })
    }

    fn slicer(sample: Arc<Sample>) -> Slicer {
        let mut s = Slicer::new();
        s.set_sample_rate(48_000.0);
        s.set_param(P_VOLUME, 0.0);
        s.set_param(P_ATTACK, 0.0005);
        s.set_param(P_RELEASE, 0.001);
        s.set_sample(sample);
        s
    }

    #[test]
    fn silent_without_sample() {
        let mut s = Slicer::new();
        s.note_on(60, 1.0);
        assert_eq!(peak(&mut s, 512), 0.0);
    }

    #[test]
    fn plays_and_releases() {
        let mut s = slicer(steps());
        s.note_on(60, 1.0);
        assert!(peak(&mut s, 400) > 0.05);
        s.note_off(60);
        render(&mut s, 2_400);
        assert_eq!(peak(&mut s, 512), 0.0);
        assert!(s.voices.iter().all(|v| !v.playing));
    }

    #[test]
    fn note_61_plays_slice_1() {
        let mut s = slicer(steps());
        assert_eq!(s.slice_bounds(), &[0, 1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000, 8_000]);
        s.note_on(59, 1.0);
        s.note_on(68, 1.0);
        assert_eq!(peak(&mut s, 64), 0.0, "hors des tranches");
        s.note_on(61, 1.0);
        let v = s.voices.iter().find(|v| v.playing).unwrap();
        assert_eq!((v.start, v.end), (1_000, 2_000));
        let (l, _) = render(&mut s, 600);
        for x in &l[100..500] {
            assert!((x - 0.2).abs() < 1e-3, "{x}");
        }
        // La tranche s'arrête d'elle-même à sa fin.
        render(&mut s, 600);
        assert_eq!(peak(&mut s, 64), 0.0);
    }

    #[test]
    fn finds_transients() {
        let mut noise = WhiteNoise::new(7);
        let hits = [0usize, 10_000, 25_000, 31_000];
        let mut left = vec![0.0f32; 40_000];
        for &h in &hits {
            for i in 0..4_000 {
                left[h + i] = noise.next_sample() * 0.8 * (-(i as f32) / 600.0).exp();
            }
        }
        let mut s = slicer(Arc::new(Sample { name: "coups".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() }));
        s.set_param(P_MODE, 1.0);
        let b = s.slice_bounds();
        assert_eq!(b.len(), hits.len() + 1, "{b:?}");
        for (got, want) in b.iter().zip(&hits) {
            assert!(got.abs_diff(*want) < 16, "{b:?}");
        }
        // Quatre tranches au plus : on garde les attaques les plus franches.
        s.set_param(P_SLICES, 0.0);
        assert!(s.slice_bounds().len() <= 5);
        s.note_on(62, 1.0);
        assert!(peak(&mut s, 1_000) > 0.05);
    }

    #[test]
    fn follows_sample_markers() {
        let mut sample = (*steps()).clone();
        sample.markers = vec![0, 1_500, 1_500, 6_000, 9_999];
        let mut s = slicer(Arc::new(sample));
        s.set_param(P_MODE, 2.0);
        assert_eq!(s.slice_bounds(), &[0, 1_500, 6_000, 8_000]);
        s.note_on(61, 1.0);
        let v = s.voices.iter().find(|v| v.playing).unwrap();
        assert_eq!((v.start, v.end), (1_500, 6_000));
        // Sans marqueurs : parts égales.
        let mut s = slicer(steps());
        s.set_param(P_MODE, 2.0);
        assert_eq!(s.slice_bounds().len(), 9);
    }

    #[test]
    fn reverse_starts_from_the_slice_end() {
        let mut s = slicer(steps());
        s.set_param(P_REVERSE, 1.0);
        s.note_on(61, 1.0);
        let (l, _) = render(&mut s, 300);
        assert!((l[200] - 0.2).abs() < 1e-3, "{}", l[200]);
    }

    #[test]
    fn survives_extremes() {
        let mut s = slicer(steps());
        stress(&mut s);
    }
}
