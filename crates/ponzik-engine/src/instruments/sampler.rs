//! Sampler : joue un échantillon chargé, transposé selon la note.
//!
//! Interpolation cubique d'Hermite, conversion de fréquence d'échantillonnage
//! intégrée au pas de lecture. Les positions sont exprimées dans le sens de
//! lecture : en mode inversé, « début » désigne la fin de la zone.

use std::sync::Arc;

use ponzik_dsp::env::{Adsr, AdsrParams};
use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::util::{db_to_gain, gain_to_db};

use super::common::{pick_voice, read_cubic, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};
use crate::sample::Sample;

const VOICES: usize = 16;
/// Au-delà, le filtre est court-circuité.
const OPEN_CUTOFF: f32 = 19_999.0;

const MODES: &[&str] = &["One-shot", "Melodic"];
const OFF_ON: &[&str] = &["No", "Yes"];

const P_ROOT: u32 = 0;
const P_MODE: u32 = 1;
const P_START: u32 = 2;
const P_END: u32 = 3;
const P_LOOP: u32 = 4;
const P_LOOP_START: u32 = 5;
const P_REVERSE: u32 = 6;
const P_ATTACK: u32 = 7;
const P_DECAY: u32 = 8;
const P_SUSTAIN: u32 = 9;
const P_RELEASE: u32 = 10;
const P_TRANSPOSE: u32 = 11;
const P_CENTS: u32 = 12;
const P_CUTOFF: u32 = 13;
const P_VOLUME: u32 = 14;
const P_SYNC: u32 = 15;
const P_BEATS: u32 = 16;

/// « Mode » : 0 one-shot (the whole sample, whatever the note length), 1 melodic.
pub const SAMPLER_MODE: u32 = P_MODE;
/// « Tempo Sync » : the sample is stretched to the project tempo (by the
/// project, which hands the stretched sample over: see `Project::synced_sample`).
pub const SAMPLER_SYNC: u32 = P_SYNC;
/// Length of the sample in beats at its own tempo, for the tempo sync.
pub const SAMPLER_BEATS: u32 = P_BEATS;

static PARAMS: [ParamInfo; 17] = [
    ParamInfo::new(P_ROOT, "Root Note", 0.0, 127.0, 60.0, Unit::None).group("Sample"),
    ParamInfo::choice(P_MODE, "Mode", MODES, 1).group("Sample"),
    ParamInfo::new(P_START, "Start", 0.0, 1.0, 0.0, Unit::Percent).group("Sample"),
    ParamInfo::new(P_END, "End", 0.0, 1.0, 1.0, Unit::Percent).group("Sample"),
    ParamInfo::choice(P_REVERSE, "Reverse", OFF_ON, 0).group("Sample"),
    ParamInfo::choice(P_SYNC, "Tempo Sync", OFF_ON, 0).group("Tempo"),
    ParamInfo::new(P_BEATS, "Beats", 0.0, 128.0, 0.0, Unit::None).group("Tempo"),
    ParamInfo::choice(P_LOOP, "Loop", OFF_ON, 0).group("Loop"),
    ParamInfo::new(P_LOOP_START, "Loop Start", 0.0, 1.0, 0.0, Unit::Percent).group("Loop"),
    ParamInfo::new(P_TRANSPOSE, "Transpose", -24.0, 24.0, 0.0, Unit::Semitones).group("Pitch"),
    ParamInfo::new(P_CENTS, "Fine Tune", -100.0, 100.0, 0.0, Unit::Cents).group("Pitch"),
    ParamInfo::new(P_CUTOFF, "Cutoff", 30.0, 20_000.0, 20_000.0, Unit::Hz).log().group("Filter"),
    ParamInfo::new(P_ATTACK, "Attack", 0.001, 4.0, 0.001, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_DECAY, "Decay", 0.005, 8.0, 0.2, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_SUSTAIN, "Sustain", 0.0, 1.0, 1.0, Unit::Percent).group("Amp"),
    ParamInfo::new(P_RELEASE, "Release", 0.005, 8.0, 0.1, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -3.0, Unit::Db).group("Amp"),
];

struct Voice {
    note: u8,
    velocity: f32,
    started_at: u64,
    playing: bool,
    /// Position dans le sens de lecture, en images depuis le début de la zone.
    pos: f64,
    env: Adsr,
    filter: [Svf; 2],
}

/// Sampler polyphonique seize voix ; muet tant qu'aucun échantillon n'est chargé.
pub struct Sampler {
    sample: Option<Arc<Sample>>,
    root: f32,
    melodic: bool,
    start: f32,
    end: f32,
    looping: bool,
    loop_start: f32,
    reverse: bool,
    env: AdsrParams,
    transpose: f32,
    cents: f32,
    cutoff: f32,
    gain: f32,
    sync: bool,
    beats: f32,
    sample_rate: f32,
    voices: [Voice; VOICES],
    note_counter: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let env = AdsrParams { attack: 0.001, decay: 0.2, sustain: 1.0, release: 0.1 };
        let mut s = Self {
            sample: None,
            root: 60.0,
            melodic: true,
            start: 0.0,
            end: 1.0,
            looping: false,
            loop_start: 0.0,
            reverse: false,
            env,
            transpose: 0.0,
            cents: 0.0,
            cutoff: 20_000.0,
            gain: 1.0,
            sync: false,
            beats: 0.0,
            sample_rate: sr,
            voices: std::array::from_fn(|_| Voice {
                note: 0,
                velocity: 0.0,
                started_at: 0,
                playing: false,
                pos: 0.0,
                env: Adsr::new(env, sr),
                filter: std::array::from_fn(|_| Svf::new(FilterMode::LowPass, 20_000.0, 0.707, sr)),
            }),
            note_counter: 0,
        };
        for p in &PARAMS {
            s.set_param(p.id, p.default);
        }
        s
    }

    pub fn sample(&self) -> Option<&Arc<Sample>> {
        self.sample.as_ref()
    }
}

impl Params for Sampler {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        let b = |x: bool| f32::from(u8::from(x));
        match id {
            P_ROOT => self.root,
            P_MODE => b(self.melodic),
            P_START => self.start,
            P_END => self.end,
            P_LOOP => b(self.looping),
            P_LOOP_START => self.loop_start,
            P_REVERSE => b(self.reverse),
            P_ATTACK => self.env.attack,
            P_DECAY => self.env.decay,
            P_SUSTAIN => self.env.sustain,
            P_RELEASE => self.env.release,
            P_TRANSPOSE => self.transpose,
            P_CENTS => self.cents,
            P_CUTOFF => self.cutoff,
            P_VOLUME => gain_to_db(self.gain),
            P_SYNC => b(self.sync),
            P_BEATS => self.beats,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_ROOT => self.root = v.round(),
            P_MODE => self.melodic = v > 0.5,
            P_START => self.start = v,
            P_END => self.end = v,
            P_LOOP => self.looping = v > 0.5,
            P_LOOP_START => self.loop_start = v,
            P_REVERSE => self.reverse = v > 0.5,
            P_ATTACK => self.env.attack = v,
            P_DECAY => self.env.decay = v,
            P_SUSTAIN => self.env.sustain = v,
            P_RELEASE => self.env.release = v,
            P_TRANSPOSE => self.transpose = v,
            P_CENTS => self.cents = v,
            P_CUTOFF => self.cutoff = v,
            P_VOLUME => self.gain = db_to_gain(v),
            P_SYNC => self.sync = v > 0.5,
            P_BEATS => self.beats = v.round(),
            _ => {}
        }
        let env = self.env;
        for voice in &mut self.voices {
            voice.env.params = env;
        }
    }
}

impl Instrument for Sampler {
    fn kind(&self) -> &'static str {
        "sampler"
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
        self.sample = Some(sample);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        if self.sample.as_ref().is_none_or(|s| s.is_empty()) {
            return;
        }
        let idx = pick_voice(VOICES, |i| self.voices[i].playing, |i| self.voices[i].started_at);
        self.note_counter += 1;
        let v = &mut self.voices[idx];
        if !v.playing {
            v.filter.iter_mut().for_each(Svf::reset);
        }
        v.note = note;
        v.velocity = velocity.clamp(0.0, 1.0);
        v.started_at = self.note_counter;
        v.playing = true;
        v.pos = 0.0;
        v.env.gate_on();
    }

    fn note_off(&mut self, note: u8) {
        // En one-shot, l'échantillon joue jusqu'au bout.
        if !self.melodic {
            return;
        }
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
        if sample.is_empty() {
            return;
        }
        let last = (sample.len() - 1) as f64;
        let (a, b) = (self.start as f64 * last, self.end as f64 * last);
        let (lo, hi) = (a.min(b), a.max(b));
        let region = hi - lo;
        let looping = self.looping && self.melodic;
        let loop_at = self.loop_start as f64 * region;
        let loop_len = region - loop_at;
        let reverse = self.reverse;
        let base = sample.sample_rate as f64 / self.sample_rate as f64
            * 2f64.powf((self.transpose as f64 + self.cents as f64 / 100.0 - self.root as f64) / 12.0);
        let filtered = self.cutoff < OPEN_CUTOFF;
        let mut rates = [0.0f64; VOICES];
        for (v, rate) in self.voices.iter_mut().zip(&mut rates).filter(|(v, _)| v.playing) {
            if region < 1.0 {
                v.playing = false;
                v.env.reset();
                continue;
            }
            *rate = base * 2f64.powf(v.note as f64 / 12.0);
            if filtered {
                for f in &mut v.filter {
                    f.set_params(self.cutoff, 0.707, self.sample_rate);
                }
            }
        }
        if self.voices.iter().all(|v| !v.playing) {
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut ml, mut mr) = (0.0, 0.0);
            for (v, &rate) in self.voices.iter_mut().zip(&rates).filter(|(v, _)| v.playing) {
                let at = if reverse { hi - v.pos } else { lo + v.pos };
                let (mut sl, mut sr) = (read_cubic(&sample.left, at), read_cubic(&sample.right, at));
                if filtered {
                    sl = v.filter[0].process(sl);
                    sr = v.filter[1].process(sr);
                }
                let amp = v.env.next_sample() * v.velocity;
                ml += sl * amp;
                mr += sr * amp;
                v.pos += rate;
                if v.pos >= region {
                    if looping && loop_len >= 1.0 {
                        v.pos = loop_at + (v.pos - region) % loop_len;
                    } else {
                        v.playing = false;
                    }
                }
                if !v.env.is_active() {
                    v.playing = false;
                }
                if !v.playing {
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
    use super::super::common::test_util::{peak, render, rising_crossings, stress};
    use super::*;

    fn sine(hz: f32, sample_rate: f32, seconds: f32) -> Arc<Sample> {
        let n = (sample_rate * seconds) as usize;
        let left: Vec<f32> =
            (0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / sample_rate).sin() * 0.8).collect();
        Arc::new(Sample { name: "sinus".into(), sample_rate, right: left.clone(), left, markers: Vec::new() })
    }

    #[test]
    fn silent_without_sample() {
        let mut s = Sampler::new();
        s.note_on(60, 1.0);
        assert_eq!(peak(&mut s, 512), 0.0);
    }

    #[test]
    fn plays_and_releases() {
        let mut s = Sampler::new();
        s.set_sample(sine(441.0, 44_100.0, 2.0));
        s.note_on(60, 1.0);
        assert!(peak(&mut s, 4_800) > 0.05);
        s.note_off(60);
        peak(&mut s, 24_000);
        assert_eq!(peak(&mut s, 512), 0.0);
        assert!(s.voices.iter().all(|v| !v.playing));
    }

    #[test]
    fn root_key_plays_at_original_rate() {
        let mut s = Sampler::new();
        s.set_sample_rate(48_000.0);
        s.set_sample(sine(441.0, 44_100.0, 3.0));
        s.note_on(60, 1.0);
        let (l, _) = render(&mut s, 48_000);
        let n = rising_crossings(&l);
        assert!((440..=442).contains(&n), "{n}");
        s.all_notes_off();
        s.note_on(72, 1.0);
        let (l, _) = render(&mut s, 24_000);
        let n = rising_crossings(&l);
        assert!((440..=442).contains(&n), "octave : {n}");
    }

    #[test]
    fn one_shot_ignores_release_and_stops_at_end() {
        let mut s = Sampler::new();
        s.set_param(P_MODE, 0.0);
        s.set_sample(sine(441.0, 48_000.0, 0.1));
        s.note_on(60, 1.0);
        s.note_off(60);
        assert!(peak(&mut s, 2_400) > 0.05);
        peak(&mut s, 4_800);
        assert_eq!(peak(&mut s, 512), 0.0);
    }

    #[test]
    fn loops_and_reverses() {
        let mut s = Sampler::new();
        s.set_param(P_LOOP, 1.0);
        s.set_param(P_LOOP_START, 0.5);
        s.set_param(P_REVERSE, 1.0);
        s.set_sample(sine(441.0, 48_000.0, 0.1));
        s.note_on(60, 1.0);
        render(&mut s, 48_000);
        assert!(peak(&mut s, 2_400) > 0.05, "la boucle s'est arrêtée");
    }

    #[test]
    fn survives_extremes() {
        let mut s = Sampler::new();
        s.set_sample(sine(441.0, 8_000.0, 0.5));
        stress(&mut s);
    }
}
