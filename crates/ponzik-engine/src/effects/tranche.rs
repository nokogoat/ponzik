use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::smooth::Smoother;

use super::{DEFAULT_SR, MIN_BPM, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_MODE: u32 = 0;
const P_DIVISION: u32 = 1;
const P_PATTERN: u32 = 2;
const P_CYCLE: u32 = 3;
const P_SMOOTHING: u32 = 4;
const P_MIX: u32 = 5;

const MODES: &[&str] = &["Hachoir", "Répétition", "Ralenti"];
const DIVISIONS: &[&str] = &["1/32", "1/16", "1/8", "1/4"];
const DIVISION_BEATS: [f32; 4] = [0.125, 0.25, 0.5, 1.0];
const CYCLES: &[&str] = &["1/4", "1/2", "1 mesure"];
const CYCLE_BEATS: [f32; 3] = [1.0, 2.0, 4.0];
const PATTERNS: &[&str] = &["Alterné", "Pointé", "Syncopé", "Rafale"];
/// Motifs du hachoir sur 16 pas, bit de poids faible = premier pas.
const PATTERN_BITS: [u16; 4] =
    [0b0101_0101_0101_0101, 0b0110_1101_1011_0110, 0b1001_0010_0100_1001, 0b0010_1111_0000_1111];

static PARAMS: [ParamInfo; 6] = [
    ParamInfo::choice(P_MODE, "Mode", MODES, 0).group("Tranche"),
    ParamInfo::choice(P_DIVISION, "Pas", DIVISIONS, 1).group("Rythme"),
    ParamInfo::choice(P_PATTERN, "Motif", PATTERNS, 0).group("Rythme"),
    ParamInfo::choice(P_CYCLE, "Cycle", CYCLES, 2).group("Rythme"),
    ParamInfo::new(P_SMOOTHING, "Lissage", 0.001, 0.05, 0.004, Unit::Seconds).log().group("Tranche"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Sortie"),
];

/// Pas le plus long au tempo le plus lent (tranche de répétition).
const MAX_SLICE: f32 = 60.0 / MIN_BPM;
/// Retard maximal du ralenti : la moitié du cycle le plus long.
const MAX_LAG: f32 = 0.5 * 4.0 * 60.0 / MIN_BPM;

/// Effets rythmiques synchronisés au tempo, répétés à chaque cycle :
/// - Hachoir : porte qui suit un motif de 16 pas ;
/// - Répétition : capture la première tranche du cycle et la rejoue jusqu'à sa fin ;
/// - Ralenti : arrêt de bande, la vitesse tombe de 1 à 0 sur le cycle.
///
/// Comme pour la pompe, le cycle tourne librement au tempo (non calé sur le transport).
pub struct Tranche {
    values: Values<6>,
    sample_rate: f32,
    bpm: f32,
    /// Position dans le cycle, en échantillons.
    pos: f32,
    slice: [Vec<f32>; 2],
    tape: [DelayLine; 2],
    gate: Smoother,
    mix: Smoother,
}

impl Default for Tranche {
    fn default() -> Self {
        let mut t = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            bpm: 120.0,
            pos: 0.0,
            slice: [Vec::new(), Vec::new()],
            tape: [DelayLine::new(2), DelayLine::new(2)],
            gate: Smoother::new(1.0),
            mix: Smoother::new(1.0),
        };
        t.set_sample_rate(DEFAULT_SR);
        t
    }
}

impl Tranche {
    fn update(&mut self) {
        self.gate.set_time(self.values.get(P_SMOOTHING) / 3.0, self.sample_rate);
        self.mix.set_target(self.values.get(P_MIX));
    }

    fn beats_to_samples(&self, beats: f32) -> f32 {
        beats * 60.0 / self.bpm * self.sample_rate
    }
}

impl Params for Tranche {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let mode = self.values.index(P_MODE);
        if self.values.set(&PARAMS, id, value) {
            if self.values.index(P_MODE) != mode {
                self.pos = 0.0;
            }
            self.update();
        }
    }
}

impl Effect for Tranche {
    fn kind(&self) -> &'static str {
        "tranche"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let slice = (MAX_SLICE * sample_rate) as usize + 4;
        self.slice = [vec![0.0; slice], vec![0.0; slice]];
        let lag = (MAX_LAG * sample_rate) as usize + 8;
        self.tape = [DelayLine::new(lag), DelayLine::new(lag)];
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.pos = 0.0;
        self.slice.iter_mut().for_each(|s| s.fill(0.0));
        self.tape.iter_mut().for_each(DelayLine::clear);
        self.gate.set_target(1.0);
        self.gate.snap();
        self.mix.snap();
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm.clamp(MIN_BPM, 999.0);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let v = &self.values;
        let mode = v.index(P_MODE);
        let step = self.beats_to_samples(DIVISION_BEATS[v.index(P_DIVISION).min(3)]).max(1.0);
        let cycle = self.beats_to_samples(CYCLE_BEATS[v.index(P_CYCLE).min(2)]).max(step);
        let pattern = PATTERN_BITS[v.index(P_PATTERN).min(3)];
        let fade = (v.get(P_SMOOTHING) * self.sample_rate).max(1.0);
        let capacity = self.slice[0].len() - 1;
        // Fenêtre de fondu aux bords d'un segment de longueur `len`.
        let window = |at: f32, len: f32| (at / fade).min((len - at) / fade).clamp(0.0, 1.0);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (dry_l, dry_r) = (*l, *r);
            self.tape[0].write(dry_l);
            self.tape[1].write(dry_r);
            let (wet_l, wet_r) = match mode {
                0 => {
                    let index = (self.pos / step) as u32 % 16;
                    self.gate.set_target(if (pattern >> index) & 1 == 1 { 1.0 } else { 0.0 });
                    let g = self.gate.next_value();
                    (dry_l * g, dry_r * g)
                }
                1 => {
                    let at = self.pos % step;
                    let g = window(at, step);
                    let i = (at as usize).min(capacity);
                    if self.pos < step {
                        self.slice[0][i] = dry_l;
                        self.slice[1][i] = dry_r;
                    }
                    (self.slice[0][i] * g, self.slice[1][i] * g)
                }
                _ => {
                    // Vitesse 1 − pos/cycle : le retard accumulé vaut pos²/(2·cycle).
                    let lag = self.pos * self.pos / (2.0 * cycle) + 1.0;
                    let g = window(self.pos, cycle);
                    (self.tape[0].read_frac(lag) * g, self.tape[1].read_frac(lag) * g)
                }
            };
            let mix = self.mix.next_value();
            *l = dry_l + (wet_l - dry_l) * mix;
            *r = dry_r + (wet_r - dry_r) * mix;
            self.pos += 1.0;
            if self.pos >= cycle {
                self.pos -= cycle;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn tranche(mode: f32) -> Tranche {
        let mut t = Tranche::default();
        t.set_tempo(120.0);
        t.set_param(P_MODE, mode);
        t.set_param(P_SMOOTHING, 0.001);
        t.reset();
        t
    }

    #[test]
    fn repeat_plays_the_captured_slice() {
        let mut t = tranche(1.0);
        // 1/16 à 120 BPM = 6 000 échantillons, cycle d'une mesure = 96 000.
        let x = noise(0.5, 96_000 + 12_000, 3);
        let (y, _) = run_mono(&mut t, &x);
        for k in 1..16 {
            for i in 100..5_900 {
                assert!((y[k * 6_000 + i] - x[i]).abs() < 1e-6, "tranche {k}, {i}");
            }
        }
        // Le cycle suivant capture une nouvelle tranche.
        assert!((y[96_000 + 3_000] - x[96_000 + 3_000]).abs() < 1e-6);
        assert!((y[102_000 + 3_000] - x[96_000 + 3_000]).abs() < 1e-6);
    }

    #[test]
    fn chopper_follows_the_pattern() {
        let mut t = tranche(0.0);
        let (y, _) = run_mono(&mut t, &vec![1.0; 24_000]);
        assert!(y[3_000] > 0.99 && y[9_000] < 0.01 && y[15_000] > 0.99, "{} {} {}", y[3_000], y[9_000], y[15_000]);
    }

    #[test]
    fn tape_stop_slows_down() {
        let mut t = tranche(2.0);
        let x = sine(1_000.0, 0.5, 96_000);
        let (y, _) = run_mono(&mut t, &x);
        let crossings = |s: &[f32]| s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let (early, late) = (crossings(&y[1_000..13_000]), crossings(&y[72_000..84_000]));
        assert!(early > 200 && late < early / 2, "{early} {late}");
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Tranche::default()), 2.0);
    }
}
