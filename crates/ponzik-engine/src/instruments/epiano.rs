//! Piano électrique : modèle FM de lame et de diapason.
//!
//! Deux chaînes par voix sur la même fondamentale : la « lame », un sinus
//! modulé par lui-même dont l'indice suit la vélocité et retombe vite (attaque
//! qui claque fort, puis son rond), et le « diapason », une porteuse modulée
//! au rapport 14 pour la cloche métallique, plus brève encore. Les aigus
//! s'éteignent plus vite que les graves. Trémolo en panoramique automatique,
//! saturation douce en sortie.

use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::util::{db_to_gain, decay_coef, gain_to_db, midi_to_hz};

use super::common::{Expr, Tune, drive, fast_sin, pan_gains, pick_voice, soft_clip};
use crate::{Controller, Instrument, NoteExpression};
use crate::param::{ParamInfo, Params, Unit};

const VOICES: usize = 12;
const SILENCE: f32 = 1e-4;
/// Rapport du modulateur de cloche.
const BELL_RATIO: f32 = 14.0;
/// Indices de modulation maximaux, en tours.
const BODY_INDEX: f32 = 0.4;
const BELL_INDEX: f32 = 0.3;
/// Constantes de temps (s) des enveloppes de brillance et de cloche, au Do4.
const MOD_TAU: f32 = 0.3;
const BELL_TAU: f32 = 0.08;
const ATTACK: f32 = 0.0015;

const P_TONE: u32 = 0;
const P_VELOCITY: u32 = 1;
const P_BELL: u32 = 2;
const P_DECAY: u32 = 3;
const P_RELEASE: u32 = 4;
const P_TREM_RATE: u32 = 5;
const P_TREM_DEPTH: u32 = 6;
const P_DRIVE: u32 = 7;
const P_VOLUME: u32 = 8;

static PARAMS: [ParamInfo; 9] = [
    ParamInfo::new(P_TONE, "Brightness", 0.0, 1.0, 0.5, Unit::Percent).group("Timbre"),
    ParamInfo::new(P_VELOCITY, "Dynamics", 0.0, 1.0, 0.7, Unit::Percent).group("Timbre"),
    ParamInfo::new(P_BELL, "Bell", 0.0, 1.0, 0.35, Unit::Percent).group("Timbre"),
    ParamInfo::new(P_DECAY, "Decay", 0.5, 20.0, 5.0, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_RELEASE, "Release", 0.02, 2.0, 0.25, Unit::Seconds).log().group("Amp"),
    ParamInfo::new(P_TREM_RATE, "Rate", 0.1, 12.0, 4.5, Unit::Hz).log().group("Tremolo"),
    ParamInfo::new(P_TREM_DEPTH, "Depth", 0.0, 1.0, 0.3, Unit::Percent).group("Tremolo"),
    ParamInfo::new(P_DRIVE, "Saturation", 0.0, 1.0, 0.15, Unit::Percent).group("Output"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -8.0, Unit::Db).group("Output"),
];

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    note: u8,
    started_at: u64,
    active: bool,
    released: bool,
    /// Fréquence en tours par échantillon.
    inc: f32,
    phase: f32,
    bell_phase: f32,
    attack: f32,
    amp: f32,
    amp_coef: f32,
    /// Indice de la lame, décroissant.
    index: f32,
    index_coef: f32,
    bell: f32,
    bell_coef: f32,
    /// Atténue la cloche quand son modulateur approche de Nyquist.
    bell_scale: f32,
    /// Déclin des aigus plus rapide : facteur sur les temps.
    keyscale: f32,
    expr: Expr,
    tune: Tune,
}

/// Piano électrique polyphonique douze voix.
pub struct EPiano {
    tone: f32,
    velocity: f32,
    bell: f32,
    decay: f32,
    release: f32,
    trem_rate: f32,
    trem_depth: f32,
    drive: f32,
    gain: f32,
    sample_rate: f32,
    lfo: Lfo,
    voices: [Voice; VOICES],
    note_counter: u64,
    /// Pitch bend in semitones.
    bend: f32,
}

impl Default for EPiano {
    fn default() -> Self {
        Self::new()
    }
}

impl EPiano {
    pub fn new() -> Self {
        let mut e = Self {
            tone: 0.5,
            velocity: 0.7,
            bell: 0.35,
            decay: 5.0,
            release: 0.25,
            trem_rate: 4.5,
            trem_depth: 0.3,
            drive: 0.15,
            gain: 1.0,
            sample_rate: 48_000.0,
            lfo: Lfo::new(LfoShape::Sine),
            voices: [Voice::default(); VOICES],
            note_counter: 0,
            bend: 0.0,
        };
        for p in &PARAMS {
            e.set_param(p.id, p.default);
        }
        e.set_sample_rate(48_000.0);
        e
    }

    /// Coefficient d'amplitude : déclin tenu ou relâche, en temps à −60 dB.
    fn amp_coef(&self, v: &Voice) -> f32 {
        let t60 = if v.released { self.release } else { self.decay * v.keyscale };
        decay_coef(t60 / 6.9, self.sample_rate)
    }
}

impl Params for EPiano {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_TONE => self.tone,
            P_VELOCITY => self.velocity,
            P_BELL => self.bell,
            P_DECAY => self.decay,
            P_RELEASE => self.release,
            P_TREM_RATE => self.trem_rate,
            P_TREM_DEPTH => self.trem_depth,
            P_DRIVE => self.drive,
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_TONE => self.tone = v,
            P_VELOCITY => self.velocity = v,
            P_BELL => self.bell = v,
            P_DECAY => self.decay = v,
            P_RELEASE => self.release = v,
            P_TREM_RATE => {
                self.trem_rate = v;
                self.lfo.set_rate(v, self.sample_rate);
            }
            P_TREM_DEPTH => self.trem_depth = v,
            P_DRIVE => self.drive = v,
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        if matches!(id, P_DECAY | P_RELEASE) {
            for i in 0..VOICES {
                self.voices[i].amp_coef = self.amp_coef(&self.voices[i]);
            }
        }
    }
}

impl Instrument for EPiano {
    fn kind(&self) -> &'static str {
        "epiano"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.lfo.set_rate(self.trem_rate, sample_rate);
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let idx = pick_voice(VOICES, |i| self.voices[i].active, |i| self.voices[i].started_at);
        self.note_counter += 1;
        let sr = self.sample_rate;
        let vel = velocity.clamp(0.0, 1.0);
        let hz = midi_to_hz(note as f32).min(sr * 0.45);
        let keyscale = (-(note as f32 - 60.0) / 24.0).exp2().clamp(0.1, 4.0);
        // La vélocité règle l'énergie et, selon « Dynamique », la brillance.
        let bright = self.tone * (1.0 - self.velocity + self.velocity * vel * vel);
        let bell_hz = hz * BELL_RATIO;
        let was_active = self.voices[idx].active;
        let mut v = Voice {
            note,
            started_at: self.note_counter,
            active: true,
            released: false,
            inc: hz / sr,
            phase: 0.0,
            bell_phase: 0.0,
            attack: 0.0,
            amp: 0.2 + 0.8 * vel,
            amp_coef: 0.0,
            index: BODY_INDEX * bright,
            index_coef: decay_coef(MOD_TAU * keyscale, sr),
            bell: self.bell * (0.3 + 0.7 * vel),
            bell_coef: decay_coef(BELL_TAU * keyscale, sr),
            bell_scale: ((sr * 0.45 - bell_hz) / (sr * 0.1)).clamp(0.0, 1.0),
            keyscale,
            ..Voice::default()
        };
        if was_active {
            // Voix volée : on garde sa phase.
            v.phase = self.voices[idx].phase;
        }
        v.amp_coef = self.amp_coef(&v);
        self.voices[idx] = v;
    }

    fn note_off(&mut self, note: u8) {
        for i in 0..VOICES {
            let v = self.voices[i];
            if v.active && !v.released && v.note == note {
                self.voices[i].released = true;
                self.voices[i].amp_coef = self.amp_coef(&self.voices[i]);
            }
        }
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        let sr = self.sample_rate;
        if let Some(v) = self.voices.iter_mut().filter(|v| v.active && !v.released && v.note == key).max_by_key(|v| v.started_at) {
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
            v.active = false;
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.voices.iter().any(|v| v.active) {
            return;
        }
        let attack_step = 1.0 / (ATTACK * self.sample_rate).max(1.0);
        let depth = self.trem_depth * 0.5;
        let bend = self.bend;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut ml, mut mr) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.active) {
                let tine = fast_sin(v.phase + v.index * fast_sin(v.phase));
                let bell = fast_sin(v.phase + BELL_INDEX * fast_sin(v.bell_phase)) * v.bell * v.bell_scale;
                let x = (tine + bell) * v.amp * v.attack;
                let (pl, pr) = pan_gains(v.expr.pan);
                ml += x * pl;
                mr += x * pr;
                let inc = (v.inc * v.tune.ratio(v.expr.tuning.tick() + bend)).min(0.45);
                v.phase = (v.phase + inc).fract();
                v.bell_phase = (v.bell_phase + inc * BELL_RATIO).fract();
                v.attack = (v.attack + attack_step).min(1.0);
                v.amp *= v.amp_coef;
                v.index *= v.index_coef;
                v.bell *= v.bell_coef;
                if v.amp < SILENCE {
                    v.active = false;
                }
            }
            let lfo = self.lfo.next_sample();
            let sl = drive(ml * 0.5, self.drive) * self.gain;
            let sr = drive(mr * 0.5, self.drive) * self.gain;
            *l += soft_clip(sl * (1.0 - depth * (1.0 + lfo)));
            *r += soft_clip(sr * (1.0 - depth * (1.0 - lfo)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, stress};
    use super::*;

    #[test]
    fn plays_and_releases() {
        let mut e = EPiano::new();
        assert_eq!(peak(&mut e, 256), 0.0);
        e.note_on(60, 0.8);
        assert!(peak(&mut e, 4_800) > 0.05);
        e.note_off(60);
        render(&mut e, 48_000);
        assert!(e.voices.iter().all(|v| !v.active));
        assert_eq!(peak(&mut e, 512), 0.0);
    }

    /// (énergie, brillance) : RMS et rapport RMS de la dérivée / RMS du signal.
    fn measure(velocity: f32) -> (f32, f32) {
        let mut e = EPiano::new();
        e.set_param(P_TREM_DEPTH, 0.0);
        e.set_param(P_DRIVE, 0.0);
        e.note_on(48, velocity);
        let (l, _) = render(&mut e, 9_600);
        let rms = |s: &mut dyn Iterator<Item = f32>| {
            let (sum, n) = s.fold((0.0f32, 0), |(a, n), x| (a + x * x, n + 1));
            (sum / n as f32).sqrt()
        };
        let energy = rms(&mut l.iter().copied());
        let slope = rms(&mut l.windows(2).map(|w| w[1] - w[0]));
        (energy, slope / energy)
    }

    #[test]
    fn velocity_changes_brightness_and_energy() {
        let (soft_e, soft_b) = measure(0.3);
        let (hard_e, hard_b) = measure(1.0);
        assert!(hard_e > 1.5 * soft_e, "{soft_e} → {hard_e}");
        assert!(hard_b > 1.3 * soft_b, "{soft_b} → {hard_b}");
    }

    #[test]
    fn steals_voices_and_survives_extremes() {
        let mut e = EPiano::new();
        for n in 0..(VOICES as u8 * 2) {
            e.note_on(40 + n, 0.9);
        }
        assert!(e.voices.iter().all(|v| v.note >= 40 + VOICES as u8));
        stress(&mut e);
    }
}
