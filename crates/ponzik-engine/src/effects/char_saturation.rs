//! Saturation à caractère : lampe, bande, transistor ou redresseur, sur tout
//! le spectre ou sur trois bandes séparées.
//!
//! La non-linéarité tourne à double fréquence d'échantillonnage (moins de
//! repliement). En mode multibande, chaque bande a son propre drive relatif ;
//! le signal direct passe par le même passe-tout que la somme des bandes, si
//! bien que le mix parallèle reste en phase.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::filter::DcBlocker;
use ponzik_dsp::oversample::{LATENCY, Oversampler2x};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::db_to_gain;

use super::{DEFAULT_SR, OFF_ON, ThreeBand, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_CHARACTER: u32 = 0;
const P_DRIVE: u32 = 1;
const P_MULTIBAND: u32 = 2;
const P_LOW_SPLIT: u32 = 3;
const P_HIGH_SPLIT: u32 = 4;
const P_LOW_DRIVE: u32 = 5;
const P_MID_DRIVE: u32 = 6;
const P_HIGH_DRIVE: u32 = 7;
const P_TONE: u32 = 8;
const P_OUTPUT: u32 = 9;
const P_MIX: u32 = 10;

static PARAMS: [ParamInfo; 11] = [
    ParamInfo::choice(P_CHARACTER, "Caractère", &["Lampe", "Bande", "Transistor", "Redresseur"], 0).group("Saturation"),
    ParamInfo::new(P_DRIVE, "Drive", 0.0, 36.0, 9.0, Unit::Db).group("Saturation"),
    ParamInfo::choice(P_MULTIBAND, "Multibande", OFF_ON, 0).group("Bandes"),
    ParamInfo::new(P_LOW_SPLIT, "Séparation grave", 40.0, 1_000.0, 200.0, Unit::Hz).log().group("Bandes"),
    ParamInfo::new(P_HIGH_SPLIT, "Séparation aiguë", 1_000.0, 12_000.0, 3_000.0, Unit::Hz).log().group("Bandes"),
    ParamInfo::new(P_LOW_DRIVE, "Drive graves", -12.0, 12.0, 0.0, Unit::Db).group("Bandes"),
    ParamInfo::new(P_MID_DRIVE, "Drive médiums", -12.0, 12.0, 0.0, Unit::Db).group("Bandes"),
    ParamInfo::new(P_HIGH_DRIVE, "Drive aigus", -12.0, 12.0, 0.0, Unit::Db).group("Bandes"),
    ParamInfo::new(P_TONE, "Ton", -12.0, 12.0, 0.0, Unit::Db).group("Couleur"),
    ParamInfo::new(P_OUTPUT, "Sortie", -24.0, 12.0, 0.0, Unit::Db).group("Sortie"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Sortie"),
];

/// Pivot de la bascule grave/aigu du réglage « Ton ».
const TILT_HZ: f32 = 1_000.0;
/// Polarisation de la lampe : harmoniques paires.
const TUBE_BIAS: f32 = 0.3;
/// Part de l'alternance négative gardée par le redresseur.
const RECTIFIER_LEAK: f32 = 0.15;
/// Seuil négatif du transistor, plus bas que le positif (asymétrie).
const TRANSISTOR_NEG: f32 = 0.8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Character {
    Tube,
    Tape,
    Transistor,
    Rectifier,
}

const CHARACTERS: [Character; 4] = [Character::Tube, Character::Tape, Character::Transistor, Character::Rectifier];

/// Écrêtage cubique : pente 1,5 à l'origine, plat à ±1 au-delà.
#[inline]
fn cubic(x: f32) -> f32 {
    let u = x.clamp(-1.0, 1.0);
    1.5 * u - 0.5 * u * u * u
}

/// Courbe de transfert du caractère ; sortie toujours dans [−1, 1].
#[inline]
fn shape(kind: Character, x: f32) -> f32 {
    match kind {
        Character::Tube => {
            let b = TUBE_BIAS.tanh();
            ((x + TUBE_BIAS).tanh() - b) / (1.0 + b)
        }
        // Algébrique : genou très progressif, harmoniques impaires seulement.
        Character::Tape => x / (1.0 + x * x).sqrt(),
        Character::Transistor if x >= 0.0 => cubic(x),
        Character::Transistor => TRANSISTOR_NEG * cubic(x / TRANSISTOR_NEG),
        Character::Rectifier => {
            let t = x.tanh();
            if t >= 0.0 { t } else { RECTIFIER_LEAK * t }
        }
    }
}

struct Channel {
    os: [Oversampler2x; 3],
    bands: ThreeBand,
    /// Même séparation sur le direct : sa somme a la phase des bandes saturées.
    dry_bands: ThreeBand,
    dc: DcBlocker,
    tilt: [Biquad; 2],
    dry: [f32; LATENCY],
}

impl Channel {
    fn new(sample_rate: f32) -> Self {
        Self {
            os: Default::default(),
            bands: ThreeBand::default(),
            dry_bands: ThreeBand::default(),
            dc: DcBlocker::new(sample_rate),
            tilt: Default::default(),
            dry: [0.0; LATENCY],
        }
    }

    fn reset(&mut self) {
        self.os.iter_mut().for_each(Oversampler2x::reset);
        self.bands.reset();
        self.dry_bands.reset();
        self.dc.reset();
        self.tilt.iter_mut().for_each(Biquad::reset);
        self.dry = [0.0; LATENCY];
    }
}

/// Saturation à caractère suréchantillonnée 2×, simple ou trois bandes, avec mix.
pub struct CharSaturation {
    values: Values<11>,
    sample_rate: f32,
    character: Character,
    channels: [Channel; 2],
    dry_pos: usize,
    drive: Smoother,
    band_drive: [Smoother; 3],
    output: Smoother,
    mix: Smoother,
}

impl Default for CharSaturation {
    fn default() -> Self {
        let mut s = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            character: Character::Tube,
            channels: [Channel::new(DEFAULT_SR), Channel::new(DEFAULT_SR)],
            dry_pos: 0,
            drive: Smoother::new(1.0),
            band_drive: std::array::from_fn(|_| Smoother::new(1.0)),
            output: Smoother::new(1.0),
            mix: Smoother::new(1.0),
        };
        s.set_sample_rate(DEFAULT_SR);
        s
    }
}

impl CharSaturation {
    fn update(&mut self) {
        let v = &self.values;
        let sr = self.sample_rate;
        self.character = CHARACTERS[v.index(P_CHARACTER).min(CHARACTERS.len() - 1)];
        let tone = v.get(P_TONE);
        let low = BiquadCoefs::low_shelf(TILT_HZ, 0.5, -tone / 2.0, sr);
        let high = BiquadCoefs::high_shelf(TILT_HZ, 0.5, tone / 2.0, sr);
        let (lo, hi) = (v.get(P_LOW_SPLIT), v.get(P_HIGH_SPLIT));
        for ch in &mut self.channels {
            ch.tilt[0].coefs = low;
            ch.tilt[1].coefs = high;
            ch.bands.set(lo, hi, sr);
            ch.dry_bands.set(lo, hi, sr);
        }
        self.drive.set_target(db_to_gain(v.get(P_DRIVE)));
        for (s, id) in self.band_drive.iter_mut().zip([P_LOW_DRIVE, P_MID_DRIVE, P_HIGH_DRIVE]) {
            s.set_target(db_to_gain(v.get(id)));
        }
        self.output.set_target(db_to_gain(v.get(P_OUTPUT)));
        self.mix.set_target(v.get(P_MIX));
    }

    fn smoothers(&mut self) -> impl Iterator<Item = &mut Smoother> {
        [&mut self.drive, &mut self.output, &mut self.mix].into_iter().chain(&mut self.band_drive)
    }
}

impl Params for CharSaturation {
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

impl Effect for CharSaturation {
    fn kind(&self) -> &'static str {
        "char_saturation"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.channels = [Channel::new(sample_rate), Channel::new(sample_rate)];
        self.smoothers().for_each(|s| s.set_time(0.02, sample_rate));
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.channels.iter_mut().for_each(Channel::reset);
        self.dry_pos = 0;
        self.smoothers().for_each(Smoother::snap);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let kind = self.character;
        let multiband = self.values.on(P_MULTIBAND);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let drive = self.drive.next_value();
            let bands = self.band_drive.each_mut().map(|s| drive * s.next_value());
            let out = self.output.next_value();
            let mix = self.mix.next_value();
            let pos = self.dry_pos;
            for (x, ch) in [l, r].into_iter().zip(&mut self.channels) {
                let (wet, dry) = if multiband {
                    let split = ch.bands.split(*x);
                    let wet = ch
                        .os
                        .iter_mut()
                        .zip(split)
                        .zip(bands)
                        .map(|((os, b), g)| os.process(b * g, |v| shape(kind, v)));
                    (wet.sum::<f32>(), ch.dry_bands.split(*x).iter().sum::<f32>())
                } else {
                    (ch.os[0].process(*x * drive, |v| shape(kind, v)), *x)
                };
                let [low, high] = &mut ch.tilt;
                let wet = high.process(low.process(ch.dc.process(wet)));
                let dry = std::mem::replace(&mut ch.dry[pos], dry);
                *x = dry * (1.0 - mix) + wet * out * mix;
            }
            self.dry_pos = (pos + 1) % LATENCY;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;
    use crate::effects::testing::*;

    /// Amplitude de la composante à `hz`.
    fn bin(y: &[f32], hz: f32) -> f32 {
        let w = TAU * hz / SR;
        let (c, s) = y
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(c, s), (n, v)| (c + v * (w * n as f32).cos(), s + v * (w * n as f32).sin()));
        2.0 * (c * c + s * s).sqrt() / y.len() as f32
    }

    #[test]
    fn shapes_are_bounded() {
        for kind in CHARACTERS {
            for i in -1_000..=1_000 {
                let y = shape(kind, i as f32 * 0.05);
                assert!(y.abs() <= 1.0 + 1e-6, "{y}");
            }
            assert!(shape(kind, 0.0).abs() < 1e-6);
        }
    }

    #[test]
    fn tube_adds_even_harmonics_and_tape_odd_ones() {
        let harmonics = |character: f32| {
            let mut s = CharSaturation::default();
            s.set_param(P_CHARACTER, character);
            s.set_param(P_DRIVE, 12.0);
            s.reset();
            let (l, _) = run_mono(&mut s, &sine(500.0, 0.5, 24_000));
            let tail = &l[4_800..];
            (bin(tail, 500.0), bin(tail, 1_000.0), bin(tail, 1_500.0))
        };
        let (h1, h2, h3) = harmonics(0.0);
        assert!(h2 > 0.05 * h1, "lampe : {h1} {h2} {h3}");
        let (h1, h2, h3) = harmonics(1.0);
        assert!(h2 < 0.002 * h1 && h3 > 0.05 * h1, "bande : {h1} {h2} {h3}");
    }

    #[test]
    fn dry_path_is_transparent_in_both_modes() {
        for multiband in [0.0, 1.0] {
            let mut s = CharSaturation::default();
            s.set_param(P_MULTIBAND, multiband);
            s.set_param(P_MIX, 0.0);
            s.reset();
            let x = sine(300.0, 0.5, 24_000);
            let (l, _) = run_mono(&mut s, &x);
            let ratio = rms(&l[12_000..]) / rms(&x[12_000..]);
            assert!((ratio - 1.0).abs() < 0.01, "{multiband} : {ratio}");
        }
    }

    #[test]
    fn band_drive_targets_its_band() {
        // Graves poussés, aigus retenus : le grave sature, l'aigu reste presque propre.
        let distortion = |hz: f32| {
            let mut s = CharSaturation::default();
            s.set_param(P_CHARACTER, 1.0);
            s.set_param(P_MULTIBAND, 1.0);
            s.set_param(P_DRIVE, 12.0);
            s.set_param(P_LOW_DRIVE, 12.0);
            s.set_param(P_HIGH_DRIVE, -12.0);
            s.reset();
            let (l, _) = run_mono(&mut s, &sine(hz, 0.5, 24_000));
            bin(&l[4_800..], 3.0 * hz) / bin(&l[4_800..], hz)
        };
        let (low, high) = (distortion(80.0), distortion(4_000.0));
        assert!(low > 4.0 * high, "{low} {high}");
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(CharSaturation::default()), 100.0);
    }
}
