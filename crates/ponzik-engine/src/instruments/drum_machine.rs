//! Boîte à rythmes : huit pads synthétisés, chacun en trois couches
//! (transitoire, corps, bruit), avec accord, déclin et niveau par pad.
//!
//! Notes compatibles General MIDI sur 36–43 : 36 grosse caisse, 37 cerclage,
//! 38 caisse claire, 39 clap, 40 charleston ouverte, 41 tom grave,
//! 42 charleston fermée, 43 tom aigu. La charleston fermée étouffe
//! l'ouverte, comme sur une vraie charleston. Chaque pad est monophonique :
//! une relance efface la frappe précédente en 1 ms au lieu de claquer.

use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::{balance_gains, db_to_gain, decay_coef, gain_to_db};

use super::common::{drive, fast_sin, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};

const PADS: usize = 8;
/// Note du premier pad.
pub const FIRST_NOTE: u8 = 36;
const OPEN_HAT: usize = 4;
const CLOSED_HAT: usize = 6;
/// Sous −80 dB, la frappe est finie.
const SILENCE: f32 = 1e-4;
const DECLICK_TAU: f32 = 0.001;
/// Étouffement de la charleston ouverte.
const CHOKE_TAU: f32 = 0.004;
/// Fréquences des six carrés métalliques des charlestons (Hz).
const METAL_HZ: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];
/// Clap : rafales rapprochées avant la queue.
const CLAP_BURSTS: u32 = 3;
const CLAP_SPACING: f32 = 0.011;
const CLAP_BURST_TAU: f32 = 0.002_5;

const GROUPS: [&str; PADS] = [
    "Grosse caisse",
    "Cerclage",
    "Caisse claire",
    "Clap",
    "Charleston ouverte",
    "Tom grave",
    "Charleston fermée",
    "Tom aigu",
];

/// Paramètres du pad `p` : `3p` accord, `3p + 1` déclin, `3p + 2` niveau.
const fn pad_param(pad: usize, slot: u32) -> u32 {
    pad as u32 * 3 + slot
}
const SLOT_TUNE: u32 = 0;
const SLOT_DECAY: u32 = 1;
const SLOT_LEVEL: u32 = 2;
const P_DRIVE: u32 = 3 * PADS as u32;
const P_VOLUME: u32 = P_DRIVE + 1;
const N_PARAMS: usize = 3 * PADS + 2;

const fn build_params() -> [ParamInfo; N_PARAMS] {
    let mut out = [ParamInfo::new(0, "", 0.0, 0.0, 0.0, Unit::None); N_PARAMS];
    let mut p = 0;
    while p < PADS {
        let g = GROUPS[p];
        out[3 * p] = ParamInfo::new(pad_param(p, SLOT_TUNE), "Accord", -24.0, 24.0, 0.0, Unit::Semitones).group(g);
        out[3 * p + 1] = ParamInfo::new(pad_param(p, SLOT_DECAY), "Déclin", 0.25, 4.0, 1.0, Unit::None).log().group(g);
        out[3 * p + 2] = ParamInfo::new(pad_param(p, SLOT_LEVEL), "Niveau", -40.0, 6.0, 0.0, Unit::Db).group(g);
        p += 1;
    }
    out[3 * PADS] = ParamInfo::new(P_DRIVE, "Saturation", 0.0, 1.0, 0.1, Unit::Percent).group("Général");
    out[3 * PADS + 1] = ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -3.0, Unit::Db).group("Général");
    out
}

static PARAMS: [ParamInfo; N_PARAMS] = build_params();

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wave {
    Sine,
    Triangle,
    /// Six carrés désaccordés (charlestons).
    Metal,
}

/// Recette d'un pad. Les temps sont des constantes de décroissance (s).
struct Recipe {
    body_hz: f32,
    wave: Wave,
    body_level: f32,
    body_tau: f32,
    /// Chute de hauteur au départ (demi-tons) et sa constante de temps.
    pitch_drop: f32,
    pitch_tau: f32,
    noise_mode: FilterMode,
    noise_hz: f32,
    noise_q: f32,
    noise_level: f32,
    noise_tau: f32,
    click_hz: f32,
    click_level: f32,
    click_tau: f32,
    /// Rafales de bruit avant la queue (clap), 0 sinon.
    bursts: u32,
    pan: f32,
}

const fn recipe(body_hz: f32, wave: Wave, body_level: f32, body_tau: f32, pitch: (f32, f32)) -> Recipe {
    Recipe {
        body_hz,
        wave,
        body_level,
        body_tau,
        pitch_drop: pitch.0,
        pitch_tau: pitch.1,
        noise_mode: FilterMode::BandPass,
        noise_hz: 1_000.0,
        noise_q: 0.7,
        noise_level: 0.0,
        noise_tau: 0.01,
        click_hz: 3_000.0,
        click_level: 0.0,
        click_tau: 0.002,
        bursts: 0,
        pan: 0.0,
    }
}

impl Recipe {
    const fn noise(mut self, mode: FilterMode, hz: f32, q: f32, level: f32, tau: f32) -> Self {
        (self.noise_mode, self.noise_hz, self.noise_q, self.noise_level, self.noise_tau) = (mode, hz, q, level, tau);
        self
    }

    const fn click(mut self, hz: f32, level: f32, tau: f32) -> Self {
        (self.click_hz, self.click_level, self.click_tau) = (hz, level, tau);
        self
    }

    const fn bursts(mut self, bursts: u32) -> Self {
        self.bursts = bursts;
        self
    }

    const fn pan(mut self, pan: f32) -> Self {
        self.pan = pan;
        self
    }
}

const HAT_NOISE: FilterMode = FilterMode::HighPass;

static RECIPES: [Recipe; PADS] = [
    // Grosse caisse : sinus qui chute de deux octaves et demie, clic court.
    recipe(52.0, Wave::Sine, 1.0, 0.13, (30.0, 0.025)).click(3_000.0, 0.5, 0.003),
    // Cerclage : triangle bref et claquant.
    recipe(480.0, Wave::Triangle, 0.6, 0.012, (2.0, 0.003))
        .noise(FilterMode::BandPass, 1_800.0, 2.0, 0.5, 0.008)
        .click(4_000.0, 0.8, 0.001_5)
        .pan(0.2),
    // Caisse claire : peau accordée, timbre de bruit, attaque.
    recipe(185.0, Wave::Sine, 0.55, 0.04, (5.0, 0.01))
        .noise(FilterMode::BandPass, 4_000.0, 0.6, 0.9, 0.07)
        .click(2_500.0, 0.4, 0.002),
    // Clap : bruit seul, rafales gérées au rendu.
    recipe(0.0, Wave::Sine, 0.0, 0.01, (0.0, 0.01))
        .noise(FilterMode::BandPass, 1_200.0, 1.4, 1.4, 0.09)
        .bursts(CLAP_BURSTS)
        .pan(-0.1),
    // Charleston ouverte.
    recipe(1.0, Wave::Metal, 0.35, 0.25, (0.0, 0.01))
        .noise(HAT_NOISE, 7_000.0, 0.7, 0.45, 0.25)
        .click(9_000.0, 0.2, 0.002)
        .pan(0.3),
    // Tom grave.
    recipe(98.0, Wave::Sine, 0.9, 0.16, (6.0, 0.04))
        .noise(FilterMode::BandPass, 500.0, 1.0, 0.15, 0.03)
        .click(2_000.0, 0.3, 0.003)
        .pan(-0.35),
    // Charleston fermée.
    recipe(1.0, Wave::Metal, 0.35, 0.025, (0.0, 0.01))
        .noise(HAT_NOISE, 7_500.0, 0.7, 0.45, 0.02)
        .click(9_000.0, 0.25, 0.001_5)
        .pan(0.3),
    // Tom aigu.
    recipe(155.0, Wave::Sine, 0.9, 0.13, (6.0, 0.035))
        .noise(FilterMode::BandPass, 800.0, 1.0, 0.15, 0.03)
        .click(2_500.0, 0.3, 0.003)
        .pan(0.25),
];

/// Réglages d'un pad.
#[derive(Clone, Copy)]
struct PadSettings {
    tune: f32,
    decay: f32,
    level_db: f32,
}

/// La frappe en cours d'un pad.
struct Voice {
    active: bool,
    gain: f32,
    ratio: f32,
    phase: f32,
    metal: [f32; 6],
    pitch_env: f32,
    pitch_coef: f32,
    body_amp: f32,
    body_coef: f32,
    noise_amp: f32,
    noise_coef: f32,
    click_amp: f32,
    click_coef: f32,
    elapsed: u32,
    choke: f32,
    choke_coef: f32,
    choking: bool,
    tail: f32,
    declick_coef: f32,
    last: f32,
    noise: WhiteNoise,
    noise_filter: Svf,
    click_filter: Svf,
    metal_filter: Svf,
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            active: false,
            gain: 0.0,
            ratio: 1.0,
            phase: 0.0,
            metal: [0.0; 6],
            pitch_env: 0.0,
            pitch_coef: 0.0,
            body_amp: 0.0,
            body_coef: 0.0,
            noise_amp: 0.0,
            noise_coef: 0.0,
            click_amp: 0.0,
            click_coef: 0.0,
            elapsed: 0,
            choke: 1.0,
            choke_coef: 0.0,
            choking: false,
            tail: 0.0,
            declick_coef: 0.0,
            last: 0.0,
            noise: WhiteNoise::new(seed),
            noise_filter: Svf::new(FilterMode::BandPass, 1_000.0, 0.7, 48_000.0),
            click_filter: Svf::new(FilterMode::BandPass, 3_000.0, 0.8, 48_000.0),
            metal_filter: Svf::new(FilterMode::HighPass, 6_500.0, 0.7, 48_000.0),
        }
    }

    fn stop(&mut self) {
        self.active = false;
        self.tail = 0.0;
        self.last = 0.0;
        self.noise_filter.reset();
        self.click_filter.reset();
        self.metal_filter.reset();
    }

    /// Lance une frappe : coefficients calculés ici, jamais pendant le rendu.
    fn trigger(&mut self, recipe: &Recipe, settings: PadSettings, velocity: f32, gain: f32, sample_rate: f32) {
        self.tail = if self.active { self.last } else { 0.0 };
        if !self.active {
            self.noise_filter.reset();
            self.click_filter.reset();
            self.metal_filter.reset();
        }
        let sr = sample_rate;
        self.ratio = (settings.tune / 12.0).exp2();
        let decay = settings.decay;
        self.pitch_coef = decay_coef(recipe.pitch_tau, sr);
        self.body_coef = decay_coef(recipe.body_tau * decay, sr);
        self.noise_coef = decay_coef(recipe.noise_tau * decay, sr);
        self.click_coef = decay_coef(recipe.click_tau, sr);
        self.choke_coef = decay_coef(CHOKE_TAU, sr);
        self.declick_coef = decay_coef(DECLICK_TAU, sr);
        // Le bruit suit l'accord plus doucement que le corps.
        let noise_hz = recipe.noise_hz * self.ratio.sqrt();
        self.noise_filter.mode = recipe.noise_mode;
        self.noise_filter.set_params(noise_hz, recipe.noise_q, sr);
        self.click_filter.set_params(recipe.click_hz * self.ratio.sqrt(), 0.8, sr);
        self.metal_filter.set_params(6_500.0 * self.ratio.sqrt(), 0.7, sr);
        let velocity = velocity.clamp(0.0, 1.0);
        self.gain = velocity * db_to_gain(settings.level_db) * gain;
        self.active = true;
        self.phase = 0.0;
        self.metal = [0.0, 0.17, 0.43, 0.61, 0.29, 0.83];
        self.pitch_env = 1.0;
        self.body_amp = recipe.body_level;
        self.noise_amp = recipe.noise_level;
        // Plus la frappe est forte, plus l'attaque claque.
        self.click_amp = recipe.click_level * (0.5 + 0.5 * velocity);
        self.elapsed = 0;
        self.choke = 1.0;
        self.choking = false;
    }

    #[inline]
    fn next(&mut self, recipe: &Recipe, sample_rate: f32) -> f32 {
        let inv_sr = 1.0 / sample_rate;
        let body = match recipe.wave {
            Wave::Metal => {
                let mut sum = 0.0;
                for (phase, hz) in self.metal.iter_mut().zip(METAL_HZ) {
                    *phase = (*phase + hz * self.ratio * inv_sr).fract();
                    sum += if *phase < 0.5 { 1.0 } else { -1.0 };
                }
                self.metal_filter.process(sum / 6.0)
            }
            wave => {
                let st = recipe.pitch_drop * self.pitch_env;
                let hz = (recipe.body_hz * self.ratio * (st / 12.0).exp2()).min(0.45 * sample_rate);
                self.phase = (self.phase + hz * inv_sr).fract();
                if wave == Wave::Sine { fast_sin(self.phase) } else { 4.0 * (self.phase - 0.5).abs() - 1.0 }
            }
        } * self.body_amp;

        let white = self.noise.next_sample();
        let in_burst = (self.elapsed as f32) < recipe.bursts as f32 * CLAP_SPACING * sample_rate;
        let noise_env = if in_burst {
            let t = self.elapsed as f32 * inv_sr;
            recipe.noise_level * (-(t % CLAP_SPACING) / CLAP_BURST_TAU).exp()
        } else {
            self.noise_amp
        };
        let noise = if recipe.noise_level > 0.0 { self.noise_filter.process(white) * noise_env } else { 0.0 };
        let click =
            if self.click_amp > SILENCE { self.click_filter.process(white) * self.click_amp * 2.0 } else { 0.0 };

        let s = (body + noise + click) * self.gain * self.choke + self.tail;
        self.last = s;

        self.pitch_env *= self.pitch_coef;
        self.body_amp *= self.body_coef;
        if !in_burst {
            self.noise_amp *= self.noise_coef;
        }
        self.click_amp *= self.click_coef;
        self.tail *= self.declick_coef;
        if self.choking {
            self.choke *= self.choke_coef;
        }
        self.elapsed = self.elapsed.saturating_add(1);
        let quiet = self.body_amp.max(self.noise_amp).max(self.click_amp) * self.choke < SILENCE;
        if !in_burst && quiet && self.tail.abs() < SILENCE {
            self.stop();
        }
        s
    }
}

/// Boîte à rythmes à huit pads synthétisés sur les notes 36 à 43.
pub struct DrumMachine {
    settings: [PadSettings; PADS],
    drive: f32,
    volume: f32,
    sample_rate: f32,
    voices: [Voice; PADS],
    pans: [(f32, f32); PADS],
}

impl Default for DrumMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumMachine {
    pub fn new() -> Self {
        let mut d = Self {
            settings: [PadSettings { tune: 0.0, decay: 1.0, level_db: 0.0 }; PADS],
            drive: 0.0,
            volume: 1.0,
            sample_rate: 48_000.0,
            voices: std::array::from_fn(|i| Voice::new(0x9E37_79B9u32.wrapping_mul(i as u32 + 1))),
            pans: RECIPES.each_ref().map(|r| balance_gains(r.pan)),
        };
        for p in &PARAMS {
            d.set_param(p.id, p.default);
        }
        d
    }

    /// Pad joué par `note`, s'il y en a un.
    pub fn pad_of(note: u8) -> Option<usize> {
        let i = note.checked_sub(FIRST_NOTE)? as usize;
        (i < PADS).then_some(i)
    }
}

impl Params for DrumMachine {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_DRIVE => self.drive,
            P_VOLUME => gain_to_db(self.volume),
            id if (id as usize) < 3 * PADS => {
                let s = &self.settings[id as usize / 3];
                match id % 3 {
                    SLOT_TUNE => s.tune,
                    SLOT_DECAY => s.decay,
                    _ => s.level_db,
                }
            }
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.get(id as usize) else { return };
        let v = info.clamp(value);
        match id {
            P_DRIVE => self.drive = v,
            P_VOLUME => self.volume = db_to_gain(v),
            _ => {
                let s = &mut self.settings[id as usize / 3];
                match id % 3 {
                    SLOT_TUNE => s.tune = v,
                    SLOT_DECAY => s.decay = v,
                    _ => s.level_db = v,
                }
            }
        }
    }
}

impl Instrument for DrumMachine {
    fn kind(&self) -> &'static str {
        "drum_machine"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let Some(pad) = Self::pad_of(note) else { return };
        if pad == CLOSED_HAT {
            self.voices[OPEN_HAT].choking = true;
        }
        self.voices[pad].trigger(&RECIPES[pad], self.settings[pad], velocity, 1.0, self.sample_rate);
    }

    fn note_off(&mut self, _note: u8) {
        // Les percussions jouent toujours leur décroissance complète.
    }

    fn all_notes_off(&mut self) {
        self.voices.iter_mut().for_each(Voice::stop);
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.voices.iter().any(|v| v.active) {
            return;
        }
        let sr = self.sample_rate;
        let (amount, volume) = (self.drive, self.volume);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut sl, mut sr_) = (0.0, 0.0);
            for ((voice, recipe), (gl, gr)) in self.voices.iter_mut().zip(&RECIPES).zip(self.pans) {
                if voice.active {
                    let s = voice.next(recipe, sr);
                    sl += s * gl;
                    sr_ += s * gr;
                }
            }
            *l += soft_clip(drive(sl, amount) * volume);
            *r += soft_clip(drive(sr_, amount) * volume);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{render, rising_crossings, stress};
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn hit(note: u8) -> Vec<f32> {
        let mut d = DrumMachine::new();
        d.note_on(note, 1.0);
        render(&mut d, 24_000).0
    }

    #[test]
    fn every_pad_sounds_then_stops() {
        for note in FIRST_NOTE..FIRST_NOTE + PADS as u8 {
            let mut d = DrumMachine::new();
            d.note_on(note, 1.0);
            let (l, r) = render(&mut d, 4_800);
            let peak = l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.05 && peak <= 2.0, "{note} : {peak}");
            render(&mut d, 4 * 48_000);
            assert!(d.voices.iter().all(|v| !v.active), "{note} ne s'arrête pas");
        }
        // Hors des pads : rien.
        let mut d = DrumMachine::new();
        d.note_on(60, 1.0);
        assert!(render(&mut d, 512).0.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn kick_and_snare_differ() {
        let (kick, snare) = (hit(36), hit(38));
        let rate = |x: &[f32]| rising_crossings(&x[..4_800]);
        assert!(rate(&snare) > 3 * rate(&kick), "{} {}", rate(&kick), rate(&snare));
        let diff = kick.iter().zip(&snare).map(|(a, b)| (a - b).powi(2)).sum::<f32>().sqrt();
        assert!(diff > 1.0, "{diff}");
    }

    #[test]
    fn closed_hat_chokes_the_open_hat() {
        let mut alone = DrumMachine::new();
        alone.note_on(40, 1.0);
        let (open, _) = render(&mut alone, 24_000);

        let mut choked = DrumMachine::new();
        choked.note_on(40, 1.0);
        let (mut l, _) = render(&mut choked, 960);
        choked.note_on(42, 1.0);
        l.extend(render(&mut choked, 24_000 - 960).0);
        // De 150 à 400 ms : la fermée est finie, l'ouverte sonnerait encore.
        let (a, b) = (rms(&open[7_200..19_200]), rms(&l[7_200..19_200]));
        assert!(a > 0.01 && b < 0.02 * a, "{a} {b}");
        assert!(!choked.voices[OPEN_HAT].active);
    }

    #[test]
    fn pad_settings_stay_on_their_pad() {
        let mut d = DrumMachine::new();
        d.set_param(pad_param(0, SLOT_LEVEL), -40.0);
        d.note_on(38, 1.0);
        let (l, _) = render(&mut d, 4_800);
        assert_eq!(l, hit(38)[..4_800]);

        // Kick accordé une octave plus haut : sa fondamentale double.
        let mut d = DrumMachine::new();
        d.set_param(pad_param(0, SLOT_TUNE), 12.0);
        d.set_param(pad_param(0, SLOT_DECAY), 4.0);
        d.note_on(36, 1.0);
        let up = render(&mut d, 24_000).0;
        let (base, high) = (rising_crossings(&hit(36)[9_600..19_200]), rising_crossings(&up[9_600..19_200]));
        assert!(high.abs_diff(2 * base) <= 2, "{base} {high}");
    }

    #[test]
    fn survives_extremes() {
        stress(&mut DrumMachine::new());
    }
}
