use ponzik_dsp::env::{Adsr, AdsrParams};
use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::osc::{Oscillator, Waveform};
use ponzik_dsp::util::midi_to_hz;

use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};

const VOICES: usize = 8;

/// Réglages de Pulse, le synthé soustractif polyphonique de base.
#[derive(Clone, Copy, Debug)]
pub struct PulseParams {
    pub waveform: Waveform,
    /// Désaccord du second oscillateur, en cents.
    pub detune_cents: f32,
    pub cutoff_hz: f32,
    pub resonance: f32,
    /// Ouverture du filtre (Hz) au sommet de l'enveloppe de filtre.
    pub filter_env_hz: f32,
    pub amp_env: AdsrParams,
    pub filter_env: AdsrParams,
    pub gain: f32,
}

const WAVES: &[&str] = &["Sine", "Saw", "Square", "Triangle"];
const P_WAVE: u32 = 0;
const P_DETUNE: u32 = 1;
const P_CUTOFF: u32 = 2;
const P_RESO: u32 = 3;
const P_FENV: u32 = 4;
const P_FDECAY: u32 = 5;
const P_ATTACK: u32 = 6;
const P_DECAY: u32 = 7;
const P_SUSTAIN: u32 = 8;
const P_RELEASE: u32 = 9;
const P_VOLUME: u32 = 10;

static PARAMS: [ParamInfo; 11] = [
    ParamInfo::choice(P_WAVE, "Wave", WAVES, 1).group("Oscillators"),
    ParamInfo::new(P_DETUNE, "Detune", 0.0, 50.0, 7.0, Unit::Cents).group("Oscillators"),
    ParamInfo::new(P_CUTOFF, "Cutoff", 40.0, 16_000.0, 400.0, Unit::Hz).log().group("Filter"),
    ParamInfo::new(P_RESO, "Resonance", 0.5, 10.0, 1.2, Unit::None).log().group("Filter"),
    ParamInfo::new(P_FENV, "Envelope", 0.0, 10_000.0, 3_000.0, Unit::Hz).group("Filter"),
    ParamInfo::new(P_FDECAY, "Filter Decay", 0.01, 4.0, 0.25, Unit::Seconds).log().group("Filter"),
    ParamInfo::new(P_ATTACK, "Attack", 0.001, 2.0, 0.004, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_DECAY, "Decay", 0.005, 4.0, 0.2, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_SUSTAIN, "Sustain", 0.0, 1.0, 0.6, Unit::Percent).group("Amp"),
    ParamInfo::new(P_RELEASE, "Release", 0.005, 4.0, 0.12, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -8.0, Unit::Db).group("Amp"),
];

impl Default for Pulse {
    fn default() -> Self {
        Self::new(PulseParams::default())
    }
}

impl Params for Pulse {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        let p = &self.params;
        match id {
            P_WAVE => WAVEFORMS.iter().position(|w| *w == p.waveform).unwrap_or(1) as f32,
            P_DETUNE => p.detune_cents,
            P_CUTOFF => p.cutoff_hz,
            P_RESO => p.resonance,
            P_FENV => p.filter_env_hz,
            P_FDECAY => p.filter_env.decay,
            P_ATTACK => p.amp_env.attack,
            P_DECAY => p.amp_env.decay,
            P_SUSTAIN => p.amp_env.sustain,
            P_RELEASE => p.amp_env.release,
            P_VOLUME => ponzik_dsp::util::gain_to_db(p.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        let p = &mut self.params;
        match id {
            P_WAVE => p.waveform = WAVEFORMS[v as usize],
            P_DETUNE => p.detune_cents = v,
            P_CUTOFF => p.cutoff_hz = v,
            P_RESO => p.resonance = v,
            P_FENV => p.filter_env_hz = v,
            P_FDECAY => p.filter_env.decay = v,
            P_ATTACK => p.amp_env.attack = v,
            P_DECAY => p.amp_env.decay = v,
            P_SUSTAIN => p.amp_env.sustain = v,
            P_RELEASE => p.amp_env.release = v,
            P_VOLUME => p.gain = ponzik_dsp::util::db_to_gain(v),
            _ => {}
        }
        let (amp, filt, wave) = (p.amp_env, p.filter_env, p.waveform);
        for voice in &mut self.voices {
            voice.amp_env.params = amp;
            voice.filter_env.params = filt;
            voice.osc1.waveform = wave;
            voice.osc2.waveform = wave;
        }
    }
}

const WAVEFORMS: [Waveform; 4] = [Waveform::Sine, Waveform::Saw, Waveform::Square, Waveform::Triangle];

impl Default for PulseParams {
    fn default() -> Self {
        Self {
            waveform: Waveform::Saw,
            detune_cents: 7.0,
            cutoff_hz: 400.0,
            resonance: 1.2,
            filter_env_hz: 3_000.0,
            amp_env: AdsrParams { attack: 0.004, decay: 0.2, sustain: 0.6, release: 0.12 },
            filter_env: AdsrParams { attack: 0.001, decay: 0.25, sustain: 0.0, release: 0.2 },
            gain: 0.4,
        }
    }
}

struct Voice {
    note: u8,
    velocity: f32,
    started_at: u64,
    osc1: Oscillator,
    osc2: Oscillator,
    filter: Svf,
    amp_env: Adsr,
    filter_env: Adsr,
}

pub struct Pulse {
    params: PulseParams,
    sample_rate: f32,
    voices: Vec<Voice>,
    note_counter: u64,
}

impl Pulse {
    pub fn new(params: PulseParams) -> Self {
        let sample_rate = 48_000.0;
        let voices = (0..VOICES)
            .map(|_| Voice {
                note: 0,
                velocity: 0.0,
                started_at: 0,
                osc1: Oscillator::new(params.waveform),
                osc2: Oscillator::new(params.waveform),
                filter: Svf::new(FilterMode::LowPass, params.cutoff_hz, params.resonance, sample_rate),
                amp_env: Adsr::new(params.amp_env, sample_rate),
                filter_env: Adsr::new(params.filter_env, sample_rate),
            })
            .collect();
        Self { params, sample_rate, voices, note_counter: 0 }
    }
}

impl Instrument for Pulse {
    fn kind(&self) -> &'static str {
        "pulse"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for v in &mut self.voices {
            v.amp_env.set_sample_rate(sample_rate);
            v.filter_env.set_sample_rate(sample_rate);
        }
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        // Une voix libre, sinon on vole la plus ancienne.
        let idx = self
            .voices
            .iter()
            .position(|v| !v.amp_env.is_active())
            .unwrap_or_else(|| {
                (0..self.voices.len()).min_by_key(|&i| self.voices[i].started_at).unwrap_or(0)
            });
        self.note_counter += 1;
        let p = self.params;
        let hz = midi_to_hz(note as f32);
        let v = &mut self.voices[idx];
        v.note = note;
        v.velocity = velocity.clamp(0.0, 1.0);
        v.started_at = self.note_counter;
        v.osc1.set_frequency(hz, self.sample_rate);
        v.osc2.set_frequency(hz * 2f32.powf(p.detune_cents / 1_200.0), self.sample_rate);
        if !v.amp_env.is_active() {
            v.osc1.reset(0.0);
            v.osc2.reset(0.37);
            v.filter.reset();
        }
        v.amp_env.gate_on();
        v.filter_env.gate_on();
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
        let p = self.params;
        let sr = self.sample_rate;
        for v in self.voices.iter_mut().filter(|v| v.amp_env.is_active()) {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let cutoff = p.cutoff_hz + v.filter_env.next_sample() * p.filter_env_hz * v.velocity;
                v.filter.set_params(cutoff, p.resonance, sr);
                let raw = 0.5 * (v.osc1.next_sample() + v.osc2.next_sample());
                let s = v.filter.process(raw) * v.amp_env.next_sample() * v.velocity * p.gain;
                *l += s;
                *r += s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(synth: &mut Pulse, frames: usize) -> f32 {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        synth.render(&mut l, &mut r);
        l.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn plays_and_releases() {
        let mut synth = Pulse::new(PulseParams::default());
        synth.set_sample_rate(48_000.0);
        assert_eq!(peak(&mut synth, 512), 0.0);
        synth.note_on(45, 1.0);
        assert!(peak(&mut synth, 4_800) > 0.05);
        synth.note_off(45);
        peak(&mut synth, 48_000);
        assert_eq!(peak(&mut synth, 512), 0.0);
    }

    #[test]
    fn steals_voices_when_full() {
        let mut synth = Pulse::new(PulseParams::default());
        for n in 0..(VOICES as u8 * 2) {
            synth.note_on(40 + n, 0.8);
        }
        assert!(synth.voices.iter().all(|v| v.note >= 40 + VOICES as u8));
    }
}
