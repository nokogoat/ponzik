//! Amp & Cab: a guitar amplifier and its speaker cabinet.
//!
//! Preamp: up to three tube-like gain stages (biased, asymmetric
//! saturation: even and odd harmonics), each followed by a coupling
//! highpass and a Miller-like lowpass, all four times oversampled. Then the
//! tone stack (bass, mid, treble), the power amp (softer saturation whose
//! supply sags under load, presence) and the cabinet: a speaker modeled by
//! a cascade of resonances and the cone's breakup, a floor reflection and a
//! microphone that moves from the cap (bright) to the edge (dark).
//!
//! [`AmpCore`] is shared with the Guitar instrument's built-in amp.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::filter::OnePole;
use ponzik_dsp::oversample::{LATENCY as OS_LATENCY, Oversampler2x};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, flush_denormal};

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

pub(crate) const MODELS: &[&str] = &["Clean", "Crunch", "Lead", "Bass"];
pub(crate) const CABS: &[&str] = &["Off", "1x12 Open Back", "2x12 Combo", "4x12 Closed", "4x10 Bass"];

/// Round trip of the two nested 2× oversamplers, in samples at the base rate.
pub(crate) const LATENCY: usize = OS_LATENCY + OS_LATENCY / 2;

/// Amp settings, knobs as on the panel (0–10 for the tone controls).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AmpSettings {
    pub model: usize,
    /// Preamp gain, [0, 1].
    pub gain: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub presence: f32,
    /// Power supply sag, [0, 1].
    pub sag: f32,
    pub cab: usize,
    /// Microphone from the cone's center (0) to its edge (1).
    pub mic: f32,
}

impl Default for AmpSettings {
    fn default() -> Self {
        Self { model: 0, gain: 0.4, bass: 5.0, mid: 5.0, treble: 5.0, presence: 5.0, sag: 0.3, cab: 2, mic: 0.3 }
    }
}

struct Model {
    stages: usize,
    /// Total preamp gain at gain 0 and 1.
    gain: (f32, f32),
    /// Input highpass (tightness), Hz.
    tight: f32,
    /// Bias of each stage (asymmetry).
    bias: [f32; 3],
    /// Lowpass after each stage, Hz.
    stage_lp: f32,
    /// Mid band: center and offset in dB at the knob's middle.
    mid: (f32, f32),
    /// Power amp drive.
    power: f32,
    /// Output level.
    level: f32,
}

const MODEL_SPECS: [Model; 4] = [
    // Clean: American combo, scooped mids, headroom.
    Model {
        stages: 2,
        gain: (1.2, 9.0),
        tight: 60.0,
        bias: [0.08, 0.12, 0.0],
        stage_lp: 12_000.0,
        mid: (450.0, -4.0),
        power: 0.9,
        level: 0.9,
    },
    // Crunch: British plexi, mid push, breaks up with the pick.
    Model {
        stages: 2,
        gain: (6.0, 90.0),
        tight: 90.0,
        bias: [0.22, 0.3, 0.0],
        stage_lp: 9_000.0,
        mid: (700.0, 1.0),
        power: 1.3,
        level: 0.55,
    },
    // Lead: high gain, three stages, tight lows.
    Model {
        stages: 3,
        gain: (60.0, 2_500.0),
        tight: 150.0,
        bias: [0.2, 0.3, 0.35],
        stage_lp: 6_500.0,
        mid: (800.0, 2.0),
        power: 1.5,
        level: 0.45,
    },
    // Bass: one stage, deep lows, wide headroom.
    Model {
        stages: 1,
        gain: (1.2, 16.0),
        tight: 28.0,
        bias: [0.1, 0.0, 0.0],
        stage_lp: 8_000.0,
        mid: (500.0, -2.0),
        power: 1.0,
        level: 0.9,
    },
];

/// Cabinet filters: (kind, hz, q, dB); kind 0 peak, 1 highpass, 2 lowpass.
type CabSpec = [(u8, f32, f32, f32); 8];

const CAB_SPECS: [CabSpec; 4] = [
    // 1x12 open back: lighter lows, bright, a dip under the breakup.
    [
        (1, 80.0, 0.75, 0.0),
        (0, 125.0, 1.3, 2.0),
        (0, 420.0, 1.0, -2.5),
        (0, 2_200.0, 1.4, 4.0),
        (0, 3_600.0, 2.2, 2.5),
        (0, 4_600.0, 4.0, -6.0),
        (2, 5_800.0, 0.9, 0.0),
        (2, 7_500.0, 0.6, 0.0),
    ],
    // 2x12 combo.
    [
        (1, 72.0, 0.85, 0.0),
        (0, 110.0, 1.4, 3.0),
        (0, 600.0, 1.2, -2.0),
        (0, 1_800.0, 1.2, 3.0),
        (0, 3_000.0, 2.5, 3.0),
        (0, 3_900.0, 4.0, -5.0),
        (2, 5_200.0, 0.85, 0.0),
        (2, 6_800.0, 0.6, 0.0),
    ],
    // 4x12 closed back: thump around 100 Hz, darker top.
    [
        (1, 62.0, 1.1, 0.0),
        (0, 100.0, 1.6, 5.0),
        (0, 450.0, 1.0, -3.0),
        (0, 1_500.0, 1.5, 2.0),
        (0, 2_700.0, 2.5, 4.0),
        (0, 3_400.0, 5.0, -6.0),
        (2, 4_600.0, 0.9, 0.0),
        (2, 6_000.0, 0.6, 0.0),
    ],
    // 4x10 bass: deep, no fizz.
    [
        (1, 38.0, 0.8, 0.0),
        (0, 80.0, 1.2, 3.0),
        (0, 700.0, 1.0, -2.0),
        (0, 2_000.0, 1.5, 3.0),
        (0, 2_900.0, 3.0, -3.0),
        (0, 3_300.0, 1.0, 0.0),
        (2, 4_000.0, 0.8, 0.0),
        (2, 5_400.0, 0.6, 0.0),
    ],
];

/// Floor reflection of the cabinet: delay (s) and weight.
const REFLECTION: (f32, f32) = (0.00065, 0.22);
const REFL_LEN: usize = 128;

/// Biased tube stage: asymmetric soft saturation, 0 at rest.
#[inline]
fn triode(x: f32, bias: f32, rest: f32) -> f32 {
    let u = x + bias;
    // Grid conduction clips the positive side harder.
    let y = if u > 0.0 { u / (1.0 + u * u).sqrt() } else { u / (1.0 + 0.6 * u * u).sqrt() };
    y - rest
}

#[inline]
fn triode_rest(bias: f32) -> f32 {
    triode(bias, 0.0, 0.0)
}

/// Coefficients shared by both channels.
#[derive(Clone, Copy)]
struct Coefs {
    stage_gain: f32,
    stages: usize,
    bias: [f32; 3],
    rest: [f32; 3],
    tight: BiquadCoefs,
    coupling: f32,
    stage_lp: f32,
    tone: [BiquadCoefs; 3],
    presence: BiquadCoefs,
    power: f32,
    sag: f32,
    sag_attack: f32,
    sag_release: f32,
    cab_on: bool,
    cab: [BiquadCoefs; 9],
    level: f32,
}

struct Channel {
    outer: Oversampler2x,
    inner: Oversampler2x,
    tight: Biquad,
    coupling: [OnePole; 3],
    lowpass: [OnePole; 3],
    tone: [Biquad; 3],
    presence: Biquad,
    sag_env: f32,
    cab: [Biquad; 9],
    refl: [f32; REFL_LEN],
    refl_pos: usize,
    dc: OnePole,
}

impl Channel {
    fn new() -> Self {
        Self {
            outer: Oversampler2x::default(),
            inner: Oversampler2x::default(),
            tight: Biquad::default(),
            coupling: Default::default(),
            lowpass: Default::default(),
            tone: Default::default(),
            presence: Biquad::default(),
            sag_env: 0.0,
            cab: Default::default(),
            refl: [0.0; REFL_LEN],
            refl_pos: 0,
            dc: OnePole::default(),
        }
    }

    fn reset(&mut self) {
        self.outer.reset();
        self.inner.reset();
        self.tight.reset();
        self.coupling.iter_mut().for_each(OnePole::reset);
        self.lowpass.iter_mut().for_each(OnePole::reset);
        self.tone.iter_mut().for_each(Biquad::reset);
        self.presence.reset();
        self.sag_env = 0.0;
        self.cab.iter_mut().for_each(Biquad::reset);
        self.refl = [0.0; REFL_LEN];
        self.dc.reset();
    }

    fn set(&mut self, c: &Coefs, sr4: f32) {
        self.tight.coefs = c.tight;
        for (hp, lp) in self.coupling.iter_mut().zip(&mut self.lowpass) {
            hp.set_coef(c.coupling);
            lp.set_cutoff(c.stage_lp, sr4);
        }
        for (f, k) in self.tone.iter_mut().zip(&c.tone) {
            f.coefs = *k;
        }
        self.presence.coefs = c.presence;
        for (f, k) in self.cab.iter_mut().zip(&c.cab) {
            f.coefs = *k;
        }
    }

    #[inline]
    fn process(&mut self, c: &Coefs, x: f32, refl_delay: usize) -> f32 {
        let Self { outer, inner, tight, coupling, lowpass, tone, presence, sag_env, .. } = self;
        let y = outer.process(x, |a| {
            inner.process(a, |b| {
                let mut v = tight.process(b);
                for s in 0..c.stages {
                    v = triode(v * c.stage_gain, c.bias[s], c.rest[s]);
                    v = coupling[s].highpass(v);
                    v = lowpass[s].lowpass(v);
                }
                for f in tone.iter_mut() {
                    v = f.process(v);
                }
                // Power amp: the supply sags under load, the stage compresses and clips softly.
                let level = v.abs();
                let k = if level > *sag_env { c.sag_attack } else { c.sag_release };
                *sag_env = flush_denormal(*sag_env + (level - *sag_env) * k);
                let drive = c.power / (1.0 + c.sag * 2.5 * *sag_env);
                let p = v * drive;
                let out = p / (1.0 + p * p).sqrt() * (1.0 + 0.3 * c.sag);
                presence.process(out)
            })
        });
        let y = y - self.dc.lowpass(y);
        if !c.cab_on {
            return y * c.level;
        }
        let mut v = y;
        for f in &mut self.cab {
            v = f.process(v);
        }
        // Floor reflection: a slight comb in the response.
        let back = self.refl[(self.refl_pos + REFL_LEN - refl_delay) % REFL_LEN];
        self.refl[self.refl_pos] = v;
        self.refl_pos = (self.refl_pos + 1) % REFL_LEN;
        (v + back * REFLECTION.1) * c.level
    }
}

/// The amp itself, stereo (two independent channels).
pub(crate) struct AmpCore {
    settings: AmpSettings,
    sample_rate: f32,
    coefs: Coefs,
    channels: [Channel; 2],
    refl_delay: usize,
}

impl AmpCore {
    pub fn new() -> Self {
        let mut a = Self {
            settings: AmpSettings::default(),
            sample_rate: DEFAULT_SR,
            coefs: Coefs {
                stage_gain: 1.0,
                stages: 1,
                bias: [0.0; 3],
                rest: [0.0; 3],
                tight: BiquadCoefs::IDENTITY,
                coupling: 0.0,
                stage_lp: 10_000.0,
                tone: [BiquadCoefs::IDENTITY; 3],
                presence: BiquadCoefs::IDENTITY,
                power: 1.0,
                sag: 0.0,
                sag_attack: 0.0,
                sag_release: 0.0,
                cab_on: false,
                cab: [BiquadCoefs::IDENTITY; 9],
                level: 1.0,
            },
            channels: [Channel::new(), Channel::new()],
            refl_delay: 1,
        };
        a.set_sample_rate(DEFAULT_SR);
        a
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update();
        self.reset();
    }

    pub fn reset(&mut self) {
        self.channels.iter_mut().for_each(Channel::reset);
    }

    pub fn set(&mut self, s: AmpSettings) {
        if s != self.settings {
            self.settings = s;
            self.update();
        }
    }

    fn update(&mut self) {
        let s = self.settings;
        let sr = self.sample_rate;
        let sr4 = 4.0 * sr;
        let m = &MODEL_SPECS[s.model.min(MODEL_SPECS.len() - 1)];
        let gain = s.gain.clamp(0.0, 1.0);
        let total = m.gain.0 * (m.gain.1 / m.gain.0).powf(gain);
        let stage_gain = total.powf(1.0 / m.stages as f32);
        let knob = |v: f32| (v.clamp(0.0, 10.0) - 5.0) / 5.0;
        let tone = [
            BiquadCoefs::low_shelf(110.0, 0.7, knob(s.bass) * 12.0, sr4),
            BiquadCoefs::peaking(m.mid.0, 0.8, m.mid.1 + knob(s.mid) * 10.0, sr4),
            BiquadCoefs::high_shelf(2_500.0, 0.7, knob(s.treble) * 12.0, sr4),
        ];
        let mut cab = [BiquadCoefs::IDENTITY; 9];
        let cab_on = s.cab > 0;
        if cab_on {
            let mic = s.mic.clamp(0.0, 1.0);
            for (k, &(kind, hz, q, db)) in cab.iter_mut().zip(&CAB_SPECS[(s.cab - 1).min(CAB_SPECS.len() - 1)]) {
                *k = match kind {
                    1 => BiquadCoefs::highpass(hz, q, sr),
                    2 => BiquadCoefs::lowpass((hz * (1.0 - 0.38 * mic)).min(sr * 0.45), q, sr),
                    _ => BiquadCoefs::peaking(hz.min(sr * 0.45), q, db, sr),
                };
            }
            // The cap is bright, the edge of the cone dark.
            cab[8] = BiquadCoefs::high_shelf(3_000.0, 0.7, 3.0 - 9.0 * mic, sr);
        }
        // Loudness: louder gain stages are evened out roughly.
        let level = m.level * if s.model == 0 || s.model == 3 { 1.0 / (0.6 + 0.4 * total.sqrt()).max(1.0) } else { 1.0 };
        self.coefs = Coefs {
            stage_gain,
            stages: m.stages,
            bias: m.bias,
            rest: m.bias.map(triode_rest),
            tight: BiquadCoefs::highpass(m.tight, 0.6, sr4),
            coupling: 1.0 - (-std::f32::consts::TAU * 25.0 / sr4).exp(),
            stage_lp: m.stage_lp,
            tone,
            presence: BiquadCoefs::high_shelf(4_500.0, 0.7, knob(s.presence) * 8.0, sr4),
            power: m.power,
            sag: s.sag.clamp(0.0, 1.0),
            sag_attack: 1.0 - (-1.0 / (0.004 * sr4)).exp(),
            sag_release: 1.0 - (-1.0 / (0.18 * sr4)).exp(),
            cab_on,
            cab,
            level: level * if cab_on { 1.4 } else { 1.0 },
        };
        let c = self.coefs;
        for ch in &mut self.channels {
            ch.set(&c, sr4);
            ch.dc.set_cutoff(8.0, sr);
        }
        self.refl_delay = ((REFLECTION.0 * sr) as usize).clamp(1, REFL_LEN - 1);
    }

    /// One sample of channel `ch` (0 left, 1 right).
    #[inline]
    pub fn process(&mut self, ch: usize, x: f32) -> f32 {
        let c = &self.coefs;
        self.channels[ch & 1].process(c, x, self.refl_delay)
    }
}

const P_MODEL: u32 = 0;
const P_GAIN: u32 = 1;
const P_BASS: u32 = 2;
const P_MID: u32 = 3;
const P_TREBLE: u32 = 4;
const P_PRESENCE: u32 = 5;
const P_SAG: u32 = 6;
const P_CAB: u32 = 7;
const P_MIC: u32 = 8;
const P_OUTPUT: u32 = 9;
const P_MIX: u32 = 10;

static PARAMS: [ParamInfo; 11] = [
    ParamInfo::choice(P_MODEL, "Amp", MODELS, 1).group("Amp"),
    ParamInfo::new(P_GAIN, "Gain", 0.0, 1.0, 0.5, Unit::Percent).group("Amp"),
    ParamInfo::new(P_BASS, "Bass", 0.0, 10.0, 5.0, Unit::None).group("Tone"),
    ParamInfo::new(P_MID, "Mid", 0.0, 10.0, 5.0, Unit::None).group("Tone"),
    ParamInfo::new(P_TREBLE, "Treble", 0.0, 10.0, 5.0, Unit::None).group("Tone"),
    ParamInfo::new(P_PRESENCE, "Presence", 0.0, 10.0, 5.0, Unit::None).group("Tone"),
    ParamInfo::new(P_SAG, "Sag", 0.0, 1.0, 0.35, Unit::Percent).group("Amp"),
    ParamInfo::choice(P_CAB, "Cabinet", CABS, 2).group("Cabinet"),
    ParamInfo::new(P_MIC, "Mic Position", 0.0, 1.0, 0.3, Unit::Percent).group("Cabinet"),
    ParamInfo::new(P_OUTPUT, "Output", -24.0, 12.0, 0.0, Unit::Db).group("Output"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Output"),
];

/// Amp & Cab as an insert effect.
pub struct Amp {
    values: Values<11>,
    core: AmpCore,
    dry: [[f32; LATENCY]; 2],
    dry_pos: usize,
    output: Smoother,
    mix: Smoother,
}

impl Default for Amp {
    fn default() -> Self {
        let mut a = Self {
            values: Values::new(&PARAMS),
            core: AmpCore::new(),
            dry: [[0.0; LATENCY]; 2],
            dry_pos: 0,
            output: Smoother::new(1.0),
            mix: Smoother::new(1.0),
        };
        a.set_sample_rate(DEFAULT_SR);
        a
    }
}

impl Amp {
    fn update(&mut self) {
        let v = &self.values;
        self.core.set(AmpSettings {
            model: v.index(P_MODEL),
            gain: v.get(P_GAIN),
            bass: v.get(P_BASS),
            mid: v.get(P_MID),
            treble: v.get(P_TREBLE),
            presence: v.get(P_PRESENCE),
            sag: v.get(P_SAG),
            cab: v.index(P_CAB),
            mic: v.get(P_MIC),
        });
        self.output.set_target(db_to_gain(v.get(P_OUTPUT)));
        self.mix.set_target(v.get(P_MIX));
    }
}

impl Params for Amp {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if self.values.set(&PARAMS, id, value) {
            self.update();
        }
    }
}

impl Effect for Amp {
    fn kind(&self) -> &'static str {
        "amp"
    }

    fn latency(&self) -> usize {
        LATENCY
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.core.set_sample_rate(sample_rate);
        self.output.set_time(0.02, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.core.reset();
        self.dry = [[0.0; LATENCY]; 2];
        self.output.snap();
        self.mix.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let out = self.output.next_value();
            let mix = self.mix.next_value();
            let pos = self.dry_pos;
            for (ch, x) in [l, r].into_iter().enumerate() {
                let wet = self.core.process(ch, *x);
                let dry = std::mem::replace(&mut self.dry[ch][pos], *x);
                *x = dry * (1.0 - mix) + wet * out * mix;
            }
            self.dry_pos = (pos + 1) % LATENCY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn tone_through(model: usize, gain: f32, cab: usize) -> Vec<f32> {
        let mut a = Amp::default();
        a.set_param(P_MODEL, model as f32);
        a.set_param(P_GAIN, gain);
        a.set_param(P_CAB, cab as f32);
        // A decaying guitar-like note at −10 dBFS.
        let x: Vec<f32> =
            sine(110.0, 0.3, 48_000).iter().enumerate().map(|(i, s)| s * (-(i as f32) / 20_000.0).exp()).collect();
        run_mono(&mut a, &x).0
    }

    #[test]
    fn every_model_and_cab_is_finite_and_bounded() {
        for (model, model_name) in MODELS.iter().enumerate() {
            for (cab, cab_name) in CABS.iter().enumerate() {
                for gain in [0.0, 0.5, 1.0] {
                    let y = tone_through(model, gain, cab);
                    assert!(y.iter().all(|s| s.is_finite()), "{model} {cab} {gain}");
                    let p = peak(&y);
                    assert!(p > 0.01 && p < 2.0, "{model_name} {cab_name} {gain}: peak {p}");
                }
            }
        }
    }

    #[test]
    fn gain_adds_harmonics() {
        // Odd harmonic share of a 110 Hz sine (third harmonic over fundamental), no cabinet.
        let h3 = |model, gain| {
            let y = tone_through(model, gain, 0);
            let s = &y[4_800..4_800 + 4_800];
            let bin = |hz: f32| {
                let (mut re, mut im) = (0.0f32, 0.0f32);
                for (n, v) in s.iter().enumerate() {
                    let ph = std::f32::consts::TAU * hz * n as f32 / SR;
                    re += v * ph.cos();
                    im += v * ph.sin();
                }
                (re * re + im * im).sqrt()
            };
            (bin(330.0) + bin(220.0)) / bin(110.0)
        };
        assert!(h3(0, 0.0) < 0.1, "clean {}", h3(0, 0.0));
        assert!(h3(2, 1.0) > 3.0 * h3(0, 0.0).max(0.02), "lead {}", h3(2, 1.0));
        assert!(h3(1, 0.8) > h3(1, 0.1));
    }

    #[test]
    fn cabinet_rolls_off_the_fizz() {
        let mut a = Amp::default();
        a.set_param(P_MODEL, 0.0);
        a.set_param(P_GAIN, 0.0);
        let level_at = |a: &mut Amp, hz: f32| {
            a.reset();
            let y = run_mono(a, &sine(hz, 0.05, 24_000)).0;
            rms(&y[12_000..])
        };
        a.set_param(P_CAB, 3.0);
        let (low, high) = (level_at(&mut a, 500.0), level_at(&mut a, 10_000.0));
        assert!(high < low * 0.05, "{low} {high}");
        // Toward the edge, the top end is darker.
        a.set_param(P_MIC, 0.0);
        let bright = level_at(&mut a, 3_000.0);
        a.set_param(P_MIC, 1.0);
        let dark = level_at(&mut a, 3_000.0);
        assert!(dark < bright * 0.7, "{bright} {dark}");
    }

    #[test]
    fn dry_path_is_aligned() {
        let mut a = Amp::default();
        a.set_param(P_MIX, 0.0);
        a.reset();
        let x = sine(440.0, 0.5, 4_096);
        let (y, _) = run_mono(&mut a, &x);
        for n in LATENCY..4_096 {
            assert!((y[n] - x[n - LATENCY]).abs() < 1e-4, "{n}");
        }
    }
}
