use std::f32::consts::PI;

use ponzik_dsp::smooth::Smoother;

use super::{DEFAULT_SR, MIN_BPM, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_RATE: u32 = 0;
const P_SHAPE: u32 = 1;
const P_DEPTH: u32 = 2;
const P_OFFSET: u32 = 3;
const P_MIX: u32 = 4;

const RATES: &[&str] = &["1/4", "1/2", "1 mesure", "1/8", "1/16"];
/// Durée de chaque cadence en noires.
const RATE_BEATS: [f32; 5] = [1.0, 2.0, 4.0, 0.5, 0.25];
const SHAPES: &[&str] = &["Douce", "Sèche", "Longue", "Rebond"];

static PARAMS: [ParamInfo; 5] = [
    ParamInfo::choice(P_RATE, "Cadence", RATES, 0).group("Rythme"),
    ParamInfo::choice(P_SHAPE, "Courbe", SHAPES, 0).group("Rythme"),
    ParamInfo::new(P_DEPTH, "Profondeur", 0.0, 1.0, 0.8, Unit::Percent).group("Pompe"),
    ParamInfo::new(P_OFFSET, "Décalage", 0.0, 1.0, 0.0, Unit::Percent).group("Rythme"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Pompe"),
];

/// Atténuation dans [0, 1] à la position `t` ∈ [0, 1) du cycle (1 au début du temps).
fn duck(shape: usize, t: f32) -> f32 {
    match shape {
        0 if t < 0.6 => 0.5 * (1.0 + (PI * t / 0.6).cos()),
        0 => 0.0,
        1 if t < 0.1 => 1.0,
        1 => (-(t - 0.1) * 30.0).exp(),
        2 => (1.0 - t).powi(2),
        _ => (1.0 - t).powi(2) * (0.5 + 0.5 * (3.0 * 2.0 * PI * t).cos()),
    }
}

/// Pompage façon compression latérale, sans entrée latérale : le volume plonge à
/// chaque temps puis remonte selon la courbe choisie.
///
/// La phase tourne librement au tempo (elle n'est pas calée sur la position du
/// transport) : « Décalage » sert à la recaler à l'oreille.
pub struct Pompe {
    values: Values<5>,
    sample_rate: f32,
    bpm: f32,
    phase: f32,
    inc: f32,
    gain: Smoother,
}

impl Default for Pompe {
    fn default() -> Self {
        let mut p = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            bpm: 120.0,
            phase: 0.0,
            inc: 0.0,
            gain: Smoother::new(1.0),
        };
        p.set_sample_rate(DEFAULT_SR);
        p
    }
}

impl Pompe {
    fn update(&mut self) {
        let beats = RATE_BEATS[self.values.index(P_RATE).min(RATE_BEATS.len() - 1)];
        self.inc = self.bpm / (60.0 * beats * self.sample_rate);
    }
}

impl Params for Pompe {
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

impl Effect for Pompe {
    fn kind(&self) -> &'static str {
        "pompe"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.gain.set_time(0.0015, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.phase = 0.0;
        self.gain.set_target(1.0);
        self.gain.snap();
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm.clamp(MIN_BPM, 999.0);
        self.update();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let v = &self.values;
        let shape = v.index(P_SHAPE);
        let amount = v.get(P_DEPTH) * v.get(P_MIX);
        let offset = v.get(P_OFFSET);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let t = (self.phase - offset).rem_euclid(1.0);
            self.gain.set_target(1.0 - amount * duck(shape, t));
            let g = self.gain.next_value();
            *l *= g;
            *r *= g;
            self.phase += self.inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn shapes_start_ducked_and_recover() {
        for shape in 0..SHAPES.len() {
            assert!((duck(shape, 0.0) - 1.0).abs() < 1e-6);
            assert!(duck(shape, 0.99) < 0.01, "{shape}");
            assert!((0..100).all(|i| (0.0..=1.0).contains(&duck(shape, i as f32 / 100.0))));
        }
    }

    #[test]
    fn ducks_on_every_beat_at_120_bpm() {
        let mut p = Pompe::default();
        p.set_param(P_DEPTH, 1.0);
        p.set_tempo(120.0);
        p.reset();
        let (y, _) = run_mono(&mut p, &vec![1.0; 96_000]);
        // Une noire à 120 BPM = 0,5 s = 24 000 échantillons.
        for beat in 0..4 {
            let start = beat * 24_000;
            let window = &y[start..start + 24_000];
            // Plongée complète moins de 5 ms après le temps.
            assert!(window[240] < 0.05, "temps {beat} : {}", window[240]);
            assert!(start == 0 || y[start - 240] > 0.95, "temps {beat} : {}", y[start - 240]);
            assert!(window[18_000] > 0.95, "temps {beat} : {}", window[18_000]);
        }
    }

    #[test]
    fn offset_and_mix() {
        let mut p = Pompe::default();
        p.set_param(P_DEPTH, 1.0);
        p.set_param(P_OFFSET, 0.5);
        p.set_param(P_MIX, 0.5);
        p.set_tempo(120.0);
        p.reset();
        let (y, _) = run_mono(&mut p, &vec![1.0; 24_000]);
        assert!(y[6_000] > 0.95 && (y[12_200] - 0.5).abs() < 0.05, "{} {}", y[6_000], y[12_200]);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Pompe::default()), 2.0);
    }
}
