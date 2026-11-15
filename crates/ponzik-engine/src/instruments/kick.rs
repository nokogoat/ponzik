//! Kick : grosse caisse synthétisée, de la sub 808 au kick saturé.
//!
//! Un sinus (mêlé à une onde de « corps » plus riche) dont la hauteur chute
//! depuis une hauteur de départ, une couche de clic (rafale de bruit filtré),
//! puis saturation et passe-bas. Monophonique : chaque note relance la frappe.
//!
//! Correspondance des notes : la note 60 (Do4) joue la fondamentale de base,
//! La1 = 55 Hz ; chaque demi-ton au-dessus ou en dessous transpose d'autant
//! (note 48 → La0 27,5 Hz, note 72 → La2 110 Hz). « Accord » décale le tout.

use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::{db_to_gain, decay_coef, gain_to_db, midi_to_hz};

use super::common::{drive, fast_sin, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};

/// Décalage note → hauteur : note 60 + `NOTE_OFFSET` = 33 (La1).
const NOTE_OFFSET: f32 = -27.0;
/// Sous −80 dB, la frappe est finie.
const SILENCE: f32 = 1e-4;
/// Durée du clic et de l'anti-clic de relance, en secondes.
const CLICK_TAU: f32 = 0.004;
const DECLICK_TAU: f32 = 0.001;
const OPEN_CUTOFF: f32 = 19_999.0;

const P_TUNE: u32 = 0;
const P_PITCH: u32 = 1;
const P_PITCH_DECAY: u32 = 2;
const P_CURVE: u32 = 3;
const P_DECAY: u32 = 4;
const P_CLICK: u32 = 5;
const P_CLICK_TONE: u32 = 6;
const P_BODY: u32 = 7;
const P_DRIVE: u32 = 8;
const P_TONE: u32 = 9;
const P_VOLUME: u32 = 10;

static PARAMS: [ParamInfo; 11] = [
    ParamInfo::new(P_TUNE, "Accord", -24.0, 24.0, 0.0, Unit::Semitones).group("Hauteur"),
    ParamInfo::new(P_PITCH, "Hauteur de départ", 0.0, 72.0, 36.0, Unit::Semitones).group("Hauteur"),
    ParamInfo::new(P_PITCH_DECAY, "Chute", 0.002, 0.5, 0.04, Unit::Seconds).log().group("Hauteur"),
    ParamInfo::new(P_CURVE, "Courbe", 0.0, 1.0, 0.5, Unit::Percent).group("Hauteur"),
    ParamInfo::new(P_DECAY, "Déclin", 0.05, 4.0, 0.6, Unit::Seconds).log().group("Amplitude"),
    ParamInfo::new(P_BODY, "Corps", 0.0, 1.0, 0.2, Unit::Percent).group("Amplitude"),
    ParamInfo::new(P_CLICK, "Niveau", 0.0, 1.0, 0.3, Unit::Percent).group("Clic"),
    ParamInfo::new(P_CLICK_TONE, "Ton", 500.0, 16_000.0, 4_000.0, Unit::Hz).log().group("Clic"),
    ParamInfo::new(P_DRIVE, "Saturation", 0.0, 1.0, 0.2, Unit::Percent).group("Sortie"),
    ParamInfo::new(P_TONE, "Tonalité", 200.0, 20_000.0, 8_000.0, Unit::Hz).log().group("Sortie"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -3.0, Unit::Db).group("Sortie"),
];

/// Grosse caisse monophonique à hauteur suivant la note.
pub struct Kick {
    tune: f32,
    pitch: f32,
    pitch_decay: f32,
    curve: f32,
    decay: f32,
    click: f32,
    click_tone: f32,
    body: f32,
    drive: f32,
    tone: f32,
    gain: f32,
    sample_rate: f32,
    pitch_coef: f32,
    amp_coef: f32,
    click_coef: f32,
    declick_coef: f32,
    noise: WhiteNoise,
    click_filter: Svf,
    tone_filter: Svf,
    active: bool,
    velocity: f32,
    base_hz: f32,
    phase: f32,
    /// Enveloppe de hauteur brute, de 1 vers 0.
    pitch_env: f32,
    amp: f32,
    click_amp: f32,
    /// Dernière sortie au moment d'une relance, fondue en 1 ms.
    tail: f32,
    last: f32,
}

impl Default for Kick {
    fn default() -> Self {
        Self::new()
    }
}

impl Kick {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let mut k = Self {
            tune: 0.0,
            pitch: 36.0,
            pitch_decay: 0.04,
            curve: 0.5,
            decay: 0.6,
            click: 0.3,
            click_tone: 4_000.0,
            body: 0.2,
            drive: 0.2,
            tone: 8_000.0,
            gain: 1.0,
            sample_rate: sr,
            pitch_coef: 0.0,
            amp_coef: 0.0,
            click_coef: 0.0,
            declick_coef: 0.0,
            noise: WhiteNoise::new(0x6B1C_4A11),
            click_filter: Svf::new(FilterMode::BandPass, 4_000.0, 0.8, sr),
            tone_filter: Svf::new(FilterMode::LowPass, 8_000.0, 0.707, sr),
            active: false,
            velocity: 0.0,
            base_hz: 55.0,
            phase: 0.0,
            pitch_env: 0.0,
            amp: 0.0,
            click_amp: 0.0,
            tail: 0.0,
            last: 0.0,
        };
        for p in &PARAMS {
            k.set_param(p.id, p.default);
        }
        k.set_sample_rate(sr);
        k
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        self.pitch_coef = decay_coef(self.pitch_decay, sr);
        // « Déclin » = temps jusqu'à −60 dB.
        self.amp_coef = decay_coef(self.decay / 6.9, sr);
        self.click_coef = decay_coef(CLICK_TAU, sr);
        self.declick_coef = decay_coef(DECLICK_TAU, sr);
        self.click_filter.set_params(self.click_tone, 0.8, sr);
        self.tone_filter.set_params(self.tone, 0.707, sr);
    }
}

impl Params for Kick {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_TUNE => self.tune,
            P_PITCH => self.pitch,
            P_PITCH_DECAY => self.pitch_decay,
            P_CURVE => self.curve,
            P_DECAY => self.decay,
            P_CLICK => self.click,
            P_CLICK_TONE => self.click_tone,
            P_BODY => self.body,
            P_DRIVE => self.drive,
            P_TONE => self.tone,
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_TUNE => self.tune = v,
            P_PITCH => self.pitch = v,
            P_PITCH_DECAY => self.pitch_decay = v,
            P_CURVE => self.curve = v,
            P_DECAY => self.decay = v,
            P_CLICK => self.click = v,
            P_CLICK_TONE => self.click_tone = v,
            P_BODY => self.body = v,
            P_DRIVE => self.drive = v,
            P_TONE => self.tone = v,
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        if matches!(id, P_PITCH_DECAY | P_DECAY | P_CLICK_TONE | P_TONE) {
            self.update();
        }
    }
}

impl Instrument for Kick {
    fn kind(&self) -> &'static str {
        "kick"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        // Relance : la frappe précédente s'efface en 1 ms au lieu de claquer.
        self.tail = if self.active { self.last } else { 0.0 };
        if !self.active {
            self.click_filter.reset();
            self.tone_filter.reset();
        }
        self.active = true;
        self.velocity = velocity.clamp(0.0, 1.0);
        self.base_hz = midi_to_hz(note as f32 + NOTE_OFFSET + self.tune);
        self.phase = 0.0;
        self.pitch_env = 1.0;
        self.amp = 1.0;
        self.click_amp = 1.0;
    }

    fn note_off(&mut self, _note: u8) {
        // Une percussion joue toujours sa décroissance complète.
    }

    fn all_notes_off(&mut self) {
        self.active = false;
        self.tail = 0.0;
        self.last = 0.0;
        self.click_filter.reset();
        self.tone_filter.reset();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.active {
            return;
        }
        let sr = self.sample_rate;
        let max_hz = sr * 0.45;
        let shape = 0.3 + 2.7 * self.curve;
        let filtered = self.tone < OPEN_CUTOFF;
        let level = self.velocity * self.gain;
        let click_level = self.click * 1.5;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let st = self.pitch * self.pitch_env.powf(shape);
            let hz = (self.base_hz * (st / 12.0).exp2()).min(max_hz);
            self.phase = (self.phase + hz / sr).fract();
            let sine = fast_sin(self.phase);
            let fat = (2.5 * sine).tanh() * 1.013_7;
            let osc = (sine + (fat - sine) * self.body) * self.amp;
            let click = self.click_filter.process(self.noise.next_sample()) * self.click_amp * click_level;
            let mut s = drive(osc + click, self.drive);
            if filtered {
                s = self.tone_filter.process(s);
            }
            let s = soft_clip(s * level + self.tail);
            self.last = s;
            *l += s;
            *r += s;

            self.pitch_env *= self.pitch_coef;
            self.amp *= self.amp_coef;
            self.click_amp *= self.click_coef;
            self.tail *= self.declick_coef;
            if self.amp < SILENCE && self.click_amp < SILENCE && self.tail.abs() < SILENCE {
                self.all_notes_off();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, rising_crossings, stress};
    use super::*;

    #[test]
    fn sounds_then_stops() {
        let mut k = Kick::new();
        assert_eq!(peak(&mut k, 256), 0.0);
        k.note_on(60, 1.0);
        assert!(peak(&mut k, 4_800) > 0.2);
        k.note_off(60);
        render(&mut k, 4 * 48_000);
        assert!(!k.active);
        assert_eq!(peak(&mut k, 512), 0.0);
    }

    #[test]
    fn pitch_sweeps_down_to_the_note() {
        let mut k = Kick::new();
        k.set_param(P_CLICK, 0.0);
        k.set_param(P_DRIVE, 0.0);
        k.set_param(P_DECAY, 4.0);
        k.note_on(60, 1.0);
        let (l, _) = render(&mut k, 48_000);
        let rate = |a: usize, b: usize| rising_crossings(&l[a..b]) as f32 * 48_000.0 / (b - a) as f32;
        let early = rate(0, 1_440);
        let late = rate(24_000, 48_000);
        assert!(early > 2.0 * late, "{early} Hz → {late} Hz");
        assert!((late - 55.0).abs() < 3.0, "fondamentale : {late}");
    }

    #[test]
    fn retrigger_restarts_the_hit() {
        let mut k = Kick::new();
        k.note_on(60, 1.0);
        render(&mut k, 24_000);
        let quiet = peak(&mut k, 1_000);
        k.note_on(60, 1.0);
        assert!(peak(&mut k, 1_000) > 2.0 * quiet);
    }

    #[test]
    fn survives_extremes() {
        stress(&mut Kick::new());
    }
}
