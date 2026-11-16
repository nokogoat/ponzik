//! Assistant de mixage : écoute la piste, propose une chaîne (coupe-bas, EQ cinq
//! bandes, de-esser, compresseur, saturation, largeur, gain de sortie) et
//! n'impose rien.
//!
//! L'écoute s'accumule dans le thread audio ([`analysis`]) ; l'interface lit le
//! résultat sans verrou et calcule la suggestion ([`suggest`]). Rien ne change
//! tant que l'utilisateur ne l'applique pas : les réglages par défaut sont
//! neutres, chaque module se contourne et reste éditable, et le macro « Amount »
//! dose de neutre (0 %, sortie identique à l'entrée au bit près) à la suggestion.

pub mod analysis;
pub mod suggest;

use std::any::Any;
use std::sync::Arc;

use ponzik_dsp::biquad::{BUTTERWORTH4_Q, Biquad, BiquadCoefs};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, decay_coef, gain_to_db};

use super::{DEFAULT_SR, OFF_ON, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

pub use analysis::{Analysis, FLOOR_DB, LISTEN_SECONDS, MixShared, N_BANDS, band_center, band_edges};
pub use suggest::{PROFILES, Profile, Suggestion, detect, suggest, target_db};

pub const P_PROFILE: u32 = 0;
pub const P_LISTEN: u32 = 1;
pub const P_AMOUNT: u32 = 2;
pub const P_HP_ON: u32 = 3;
pub const P_HP_FREQ: u32 = 4;
pub const P_EQ_ON: u32 = 5;
/// Première bande d'EQ (fréquence, gain, Q) ; les quatre suivantes suivent.
/// Bande 1 : plateau grave ; 2 à 4 : cloches ; 5 : plateau aigu.
pub const P_BAND: u32 = 6;
pub const EQ_BANDS: usize = 5;
pub const P_DS_ON: u32 = 21;
pub const P_DS_FREQ: u32 = 22;
pub const P_DS_THRESHOLD: u32 = 23;
pub const P_DS_RANGE: u32 = 24;
pub const P_COMP_ON: u32 = 25;
pub const P_COMP_THRESHOLD: u32 = 26;
pub const P_COMP_RATIO: u32 = 27;
pub const P_COMP_ATTACK: u32 = 28;
pub const P_COMP_RELEASE: u32 = 29;
pub const P_COMP_MAKEUP: u32 = 30;
pub const P_SAT_ON: u32 = 31;
pub const P_SAT_DRIVE: u32 = 32;
pub const P_WIDTH_ON: u32 = 33;
pub const P_WIDTH: u32 = 34;
pub const P_OUTPUT: u32 = 35;
pub const PARAM_COUNT: usize = 36;

const G_EQ: &str = "EQ";

const fn band(id: u32, names: [&'static str; 3], hz: f32, q: f32) -> [ParamInfo; 3] {
    [
        ParamInfo::new(id, names[0], 20.0, 20_000.0, hz, Unit::Hz).log().group(G_EQ),
        ParamInfo::new(id + 1, names[1], -12.0, 12.0, 0.0, Unit::Db).group(G_EQ),
        ParamInfo::new(id + 2, names[2], 0.3, 6.0, q, Unit::None).log().group(G_EQ),
    ]
}

const B: [[ParamInfo; 3]; EQ_BANDS] = [
    band(P_BAND, ["Low shelf freq", "Low shelf gain", "Low shelf Q"], 120.0, 0.707),
    band(P_BAND + 3, ["Bell 1 freq", "Bell 1 gain", "Bell 1 Q"], 300.0, 1.0),
    band(P_BAND + 6, ["Bell 2 freq", "Bell 2 gain", "Bell 2 Q"], 1_000.0, 1.0),
    band(P_BAND + 9, ["Bell 3 freq", "Bell 3 gain", "Bell 3 Q"], 3_500.0, 1.0),
    band(P_BAND + 12, ["High shelf freq", "High shelf gain", "High shelf Q"], 9_000.0, 0.707),
];

static PARAMS: [ParamInfo; PARAM_COUNT] = [
    ParamInfo::choice(P_PROFILE, "Profile", PROFILES, 0).group("Assistant"),
    ParamInfo::choice(P_LISTEN, "Listen", OFF_ON, 0).group("Assistant"),
    ParamInfo::new(P_AMOUNT, "Amount", 0.0, 1.0, 1.0, Unit::Percent).group("Assistant"),
    ParamInfo::choice(P_HP_ON, "High-pass", OFF_ON, 0).group("High-pass"),
    ParamInfo::new(P_HP_FREQ, "High-pass freq", 20.0, 500.0, 80.0, Unit::Hz).log().group("High-pass"),
    ParamInfo::choice(P_EQ_ON, "EQ", OFF_ON, 1).group(G_EQ),
    B[0][0],
    B[0][1],
    B[0][2],
    B[1][0],
    B[1][1],
    B[1][2],
    B[2][0],
    B[2][1],
    B[2][2],
    B[3][0],
    B[3][1],
    B[3][2],
    B[4][0],
    B[4][1],
    B[4][2],
    ParamInfo::choice(P_DS_ON, "De-esser", OFF_ON, 1).group("De-esser"),
    ParamInfo::new(P_DS_FREQ, "De-ess freq", 3_000.0, 12_000.0, 6_500.0, Unit::Hz).log().group("De-esser"),
    ParamInfo::new(P_DS_THRESHOLD, "De-ess threshold", -60.0, 0.0, -30.0, Unit::Db).group("De-esser"),
    ParamInfo::new(P_DS_RANGE, "De-ess range", 0.0, 12.0, 0.0, Unit::Db).group("De-esser"),
    ParamInfo::choice(P_COMP_ON, "Compressor", OFF_ON, 1).group("Compressor"),
    ParamInfo::new(P_COMP_THRESHOLD, "Threshold", -50.0, 0.0, -18.0, Unit::Db).group("Compressor"),
    ParamInfo::new(P_COMP_RATIO, "Ratio", 1.0, 10.0, 1.0, Unit::Ratio).log().group("Compressor"),
    ParamInfo::new(P_COMP_ATTACK, "Attack", 0.000_2, 0.1, 0.01, Unit::Seconds).log().group("Compressor"),
    ParamInfo::new(P_COMP_RELEASE, "Release", 0.02, 1.0, 0.15, Unit::Seconds).log().group("Compressor"),
    ParamInfo::new(P_COMP_MAKEUP, "Makeup", 0.0, 18.0, 0.0, Unit::Db).group("Compressor"),
    ParamInfo::choice(P_SAT_ON, "Saturation", OFF_ON, 1).group("Saturation"),
    ParamInfo::new(P_SAT_DRIVE, "Drive", 0.0, 1.0, 0.0, Unit::Percent).group("Saturation"),
    ParamInfo::choice(P_WIDTH_ON, "Stereo width", OFF_ON, 1).group("Width"),
    ParamInfo::new(P_WIDTH, "Width", 0.0, 2.0, 1.0, Unit::Percent).group("Width"),
    ParamInfo::new(P_OUTPUT, "Output", -18.0, 18.0, 0.0, Unit::Db).group("Output"),
];

/// Fiches des paramètres (l'interface les retrouve par id).
pub fn params() -> &'static [ParamInfo; PARAM_COUNT] {
    &PARAMS
}

/// Coupe-bas : deux étages Butterworth (24 dB/oct).
const HP_STAGES: usize = 2;
/// Genou du compresseur (dB).
const KNEE: f32 = 6.0;
/// Ratio du de-esser.
const DS_RATIO: f32 = 4.0;

/// Coefficients du coupe-bas et des cinq bandes à `amount` (0..1), d'après une
/// lecture des réglages ; partagé par le moteur, la suggestion et l'interface.
/// Un étage `None` est neutre.
pub fn chain_filters(get: &dyn Fn(u32) -> f32, amount: f32, sample_rate: f32) -> [Option<BiquadCoefs>; HP_STAGES + EQ_BANDS] {
    let mut out = [None; HP_STAGES + EQ_BANDS];
    if amount <= 0.0 {
        return out;
    }
    if get(P_HP_ON) >= 0.5 {
        // Amount glisse la coupure, en octaves, depuis 20 Hz.
        let hz = 20.0 * (get(P_HP_FREQ).max(20.0) / 20.0).powf(amount);
        for (s, q) in BUTTERWORTH4_Q.iter().enumerate() {
            out[s] = Some(BiquadCoefs::highpass(hz, *q, sample_rate));
        }
    }
    if get(P_EQ_ON) >= 0.5 {
        for b in 0..EQ_BANDS {
            let id = P_BAND + 3 * b as u32;
            let (hz, gain, q) = (get(id), get(id + 1) * amount, get(id + 2));
            if gain == 0.0 {
                continue;
            }
            out[HP_STAGES + b] = Some(match b {
                0 => BiquadCoefs::low_shelf(hz, q, gain, sample_rate),
                4 => BiquadCoefs::high_shelf(hz, q, gain, sample_rate),
                _ => BiquadCoefs::peaking(hz, q, gain, sample_rate),
            });
        }
    }
    out
}

/// Réponse (dB) du coupe-bas et de l'EQ à `hz`.
pub fn chain_response_db(get: &dyn Fn(u32) -> f32, amount: f32, hz: f32, sample_rate: f32) -> f32 {
    chain_filters(get, amount, sample_rate)
        .iter()
        .flatten()
        .map(|c| gain_to_db(c.magnitude(hz, sample_rate)))
        .sum()
}

/// Réduction de gain (dB, positive) au niveau `x` dB, genou doux.
pub fn comp_curve(x: f32, threshold: f32, ratio: f32) -> f32 {
    let over = x - threshold;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if 2.0 * over <= -KNEE {
        0.0
    } else if 2.0 * over.abs() <= KNEE {
        slope * (over + KNEE / 2.0).powi(2) / (2.0 * KNEE)
    } else {
        slope * over
    }
}

/// Saturation douce à gain unitaire pour les petits signaux.
#[inline]
fn saturate(x: f32, k: f32) -> f32 {
    (k * x).tanh() / k
}

/// Assistant de mixage : analyse, suggestion et chaîne dosée par « Amount ».
pub struct MixAssistant {
    values: Values<PARAM_COUNT>,
    sample_rate: f32,
    analyzer: analysis::Analyzer,
    // Chaîne effective (réglages × Amount).
    amount: f32,
    filters: [[Biquad; HP_STAGES + EQ_BANDS]; 2],
    active: [bool; HP_STAGES + EQ_BANDS],
    ds_split: [Biquad; 2],
    /// Détecteur des sifflantes : passe-haut d'ordre 4, par canal.
    ds_detect: [[Biquad; 2]; 2],
    ds_on: bool,
    ds_threshold: f32,
    ds_range: f32,
    ds_attack: f32,
    ds_release: f32,
    ds_gr: f32,
    comp_on: bool,
    comp_threshold: f32,
    comp_ratio: f32,
    comp_attack: f32,
    comp_release: f32,
    comp_gr: f32,
    makeup: Smoother,
    drive: f32,
    width: f32,
    output: Smoother,
}

impl Default for MixAssistant {
    fn default() -> Self {
        let mut m = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            analyzer: analysis::Analyzer::new(DEFAULT_SR),
            amount: 1.0,
            filters: Default::default(),
            active: [false; HP_STAGES + EQ_BANDS],
            ds_split: Default::default(),
            ds_detect: Default::default(),
            ds_on: false,
            ds_threshold: 0.0,
            ds_range: 0.0,
            ds_attack: 0.0,
            ds_release: 0.0,
            ds_gr: 0.0,
            comp_on: false,
            comp_threshold: 0.0,
            comp_ratio: 1.0,
            comp_attack: 0.0,
            comp_release: 0.0,
            comp_gr: 0.0,
            makeup: Smoother::new(1.0),
            drive: 0.0,
            width: 1.0,
            output: Smoother::new(1.0),
        };
        m.set_sample_rate(DEFAULT_SR);
        m
    }
}

impl MixAssistant {
    /// État partagé avec l'interface (analyse, indicateurs).
    pub fn shared_state(&self) -> Arc<MixShared> {
        self.analyzer.shared.clone()
    }

    /// Raccourcit l'écoute (tests).
    #[cfg(test)]
    pub(crate) fn set_listen_seconds(&mut self, s: f32) {
        self.analyzer.listen_seconds = s;
    }

    fn update(&mut self) {
        let v = &self.values;
        let sr = self.sample_rate;
        let a = v.get(P_AMOUNT);
        self.amount = a;
        let get = |id| v.get(id);
        let coefs = chain_filters(&get, a, sr);
        for (s, c) in coefs.iter().enumerate() {
            let on = c.is_some();
            for ch in &mut self.filters {
                if on && !self.active[s] {
                    ch[s].reset();
                }
                ch[s].coefs = c.unwrap_or(BiquadCoefs::IDENTITY);
            }
            self.active[s] = on;
        }

        let ds_on = v.on(P_DS_ON) && v.get(P_DS_RANGE) * a > 0.01;
        let split = BiquadCoefs::lowpass(v.get(P_DS_FREQ), std::f32::consts::FRAC_1_SQRT_2, sr);
        for f in &mut self.ds_split {
            if ds_on && !self.ds_on {
                f.reset();
            }
            f.coefs = split;
        }
        for ch in &mut self.ds_detect {
            for (f, q) in ch.iter_mut().zip(BUTTERWORTH4_Q) {
                if ds_on && !self.ds_on {
                    f.reset();
                }
                f.coefs = BiquadCoefs::highpass(v.get(P_DS_FREQ), q, sr);
            }
        }
        if !ds_on {
            self.ds_gr = 0.0;
        }
        self.ds_on = ds_on;
        self.ds_threshold = v.get(P_DS_THRESHOLD);
        self.ds_range = v.get(P_DS_RANGE) * a;

        let ratio = 1.0 + (v.get(P_COMP_RATIO) - 1.0) * a;
        let makeup = v.get(P_COMP_MAKEUP) * a;
        let comp_on = v.on(P_COMP_ON) && (ratio > 1.001 || makeup > 0.0);
        if !comp_on {
            self.comp_gr = 0.0;
        }
        self.comp_on = comp_on;
        self.comp_threshold = v.get(P_COMP_THRESHOLD);
        self.comp_ratio = ratio;
        self.comp_attack = decay_coef(v.get(P_COMP_ATTACK), sr);
        self.comp_release = decay_coef(v.get(P_COMP_RELEASE), sr);
        self.makeup.set_target(if comp_on { db_to_gain(makeup) } else { 1.0 });

        self.drive = if v.on(P_SAT_ON) { v.get(P_SAT_DRIVE) * a } else { 0.0 };
        self.width = if v.on(P_WIDTH_ON) { 1.0 + (v.get(P_WIDTH) - 1.0) * a } else { 1.0 };
        self.output.set_target(db_to_gain(v.get(P_OUTPUT) * a));
    }

    fn listen_changed(&mut self, on: bool) {
        if on {
            self.analyzer.start();
        } else {
            self.analyzer.stop();
        }
    }
}

impl Params for MixAssistant {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let was = self.values.on(P_LISTEN);
        if self.values.set(&PARAMS, id, value) {
            if id == P_LISTEN && self.values.on(P_LISTEN) != was {
                self.listen_changed(!was);
            }
            self.update();
        }
    }
}

impl Effect for MixAssistant {
    fn kind(&self) -> &'static str {
        "mix_assistant"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.analyzer.set_sample_rate(sample_rate);
        self.ds_attack = decay_coef(0.001, sample_rate);
        self.ds_release = decay_coef(0.06, sample_rate);
        self.makeup.set_time(0.02, sample_rate);
        self.output.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.filters.iter_mut().flatten().for_each(Biquad::reset);
        self.ds_split.iter_mut().chain(self.ds_detect.iter_mut().flatten()).for_each(Biquad::reset);
        self.ds_gr = 0.0;
        self.comp_gr = 0.0;
        self.makeup.snap();
        self.output.snap();
        self.analyzer.reset();
    }

    fn shared(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        Some(self.analyzer.shared.clone())
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        // L'écoute porte sur l'entrée, avant la chaîne.
        self.analyzer.process(left, right);
        if self.amount <= 0.0 {
            // Neutre : la sortie est l'entrée, au bit près.
            self.analyzer.shared.set_meters(0.0, 0.0);
            return;
        }
        let active = self.active;
        let [fl, fr] = &mut self.filters;
        let (mut comp_max, mut ds_max) = (0.0f32, 0.0f32);
        let k = 1.0 + 5.0 * self.drive;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut a, mut b) = (*l, *r);
            for s in 0..active.len() {
                if active[s] {
                    a = fl[s].process(a);
                    b = fr[s].process(b);
                }
            }
            if self.ds_on {
                // Bande haute = signal − bande basse : sans réduction, la somme est exacte.
                let [sl, sr] = &mut self.ds_split;
                let (low_a, low_b) = (sl.process(a), sr.process(b));
                let (hi_a, hi_b) = (a - low_a, b - low_b);
                let [[d0, d1], [d2, d3]] = &mut self.ds_detect;
                let (da, db) = (d1.process(d0.process(a)), d3.process(d2.process(b)));
                let level = gain_to_db(da.abs().max(db.abs()));
                let target = ((level - self.ds_threshold) * (1.0 - 1.0 / DS_RATIO)).clamp(0.0, self.ds_range);
                let coef = if target > self.ds_gr { self.ds_attack } else { self.ds_release };
                self.ds_gr = target + (self.ds_gr - target) * coef;
                let g = db_to_gain(-self.ds_gr);
                a = low_a + hi_a * g;
                b = low_b + hi_b * g;
                ds_max = ds_max.max(self.ds_gr);
            }
            if self.comp_on {
                let level = gain_to_db(a.abs().max(b.abs()));
                let target = comp_curve(level, self.comp_threshold, self.comp_ratio);
                let coef = if target > self.comp_gr { self.comp_attack } else { self.comp_release };
                self.comp_gr = target + (self.comp_gr - target) * coef;
                let g = db_to_gain(-self.comp_gr) * self.makeup.next_value();
                a *= g;
                b *= g;
                comp_max = comp_max.max(self.comp_gr);
            }
            if self.drive > 0.0 {
                a = saturate(a, k);
                b = saturate(b, k);
            }
            if self.width != 1.0 {
                let (m, s) = (0.5 * (a + b), 0.5 * (a - b) * self.width);
                a = m + s;
                b = m - s;
            }
            let g = self.output.next_value();
            if g != 1.0 {
                a *= g;
                b *= g;
            }
            *l = a;
            *r = b;
        }
        self.analyzer.shared.set_meters(comp_max, ds_max);
    }
}

#[cfg(test)]
mod tests;
