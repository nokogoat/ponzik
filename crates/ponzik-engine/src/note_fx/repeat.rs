use super::{Countdown, NoteBuffer, NoteFx, RATES, step_samples};
use crate::effects::{DEFAULT_SR, Values};
use crate::param::{ParamInfo, Params, Unit};

const P_RATE: u32 = 0;
const P_COUNT: u32 = 1;
const P_GATE: u32 = 2;
const P_DECAY: u32 = 3;

/// Notes répétées en même temps ; au-delà, les nouvelles passent sans répétition.
const MAX_VOICES: usize = 32;

const COUNTS: &[&str] = &["Held", "2", "3", "4", "5", "6", "8", "12", "16"];
/// Coups par note de [`COUNTS`] ; 0 : tant que la note est tenue.
const COUNT_HITS: [u32; 9] = [0, 2, 3, 4, 5, 6, 8, 12, 16];

static PARAMS: [ParamInfo; 4] = [
    ParamInfo::choice(P_RATE, "Division", RATES, 5).group("Rhythm"),
    ParamInfo::choice(P_COUNT, "Hits", COUNTS, 0).group("Rhythm"),
    ParamInfo::new(P_GATE, "Gate Length", 0.05, 1.0, 0.5, Unit::Percent).group("Rhythm"),
    ParamInfo::new(P_DECAY, "Decay", 0.0, 1.0, 0.0, Unit::Percent).group("Velocity"),
];

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    key: u8,
    vel: f32,
    /// Coups déjà joués.
    hits: u32,
    next: Countdown,
    gate: Countdown,
    sounding: bool,
}

/// Répétition (ratchet) : chaque note est rejouée à la cadence choisie, tant
/// qu'elle est tenue (« Tenue ») ou un nombre fixe de coups, même relâchée.
/// « Déclin » baisse la vélocité à chaque coup.
pub struct Repeat {
    values: Values<4>,
    sample_rate: f32,
    bpm: f32,
    voices: [Voice; MAX_VOICES],
    len: usize,
    /// Notes passées sans répétition faute de voix libre.
    passed: [bool; 128],
}

impl Default for Repeat {
    fn default() -> Self {
        Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            bpm: 120.0,
            voices: [Voice::default(); MAX_VOICES],
            len: 0,
            passed: [false; 128],
        }
    }
}

impl Repeat {
    fn hits(&self) -> u32 {
        COUNT_HITS[self.values.index(P_COUNT).min(COUNT_HITS.len() - 1)]
    }

    fn remove(&mut self, i: usize) {
        self.voices.copy_within(i + 1..self.len, i);
        self.len -= 1;
    }

    /// Joue le prochain coup de la voix `i`, dû maintenant.
    fn strike(&mut self, i: usize, out: &mut NoteBuffer) {
        let step = step_samples(self.values.index(P_RATE), self.bpm, self.sample_rate);
        let gate = self.values.get(P_GATE) as f64;
        let decay = 1.0 - self.values.get(P_DECAY);
        let v = &mut self.voices[i];
        if v.sounding {
            out.note_off(v.key);
        }
        out.note_on(v.key, (v.vel * decay.powi(v.hits as i32)).max(0.01));
        v.sounding = true;
        v.hits += 1;
        v.gate = Countdown(v.next.0 + step * gate);
        v.next.0 += step;
    }
}

impl Params for Repeat {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        self.values.set(&PARAMS, id, value);
    }
}

impl NoteFx for Repeat {
    fn kind(&self) -> &'static str {
        "repeat"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.reset();
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm;
    }

    fn reset(&mut self) {
        self.len = 0;
        self.passed = [false; 128];
    }

    fn note_on(&mut self, key: u8, vel: f32, out: &mut NoteBuffer) {
        if let Some(i) = self.voices[..self.len].iter().position(|v| v.key == key) {
            if self.voices[i].sounding {
                out.note_off(key);
            }
            self.remove(i);
        }
        if self.len == MAX_VOICES {
            self.passed[key.min(127) as usize] = true;
            out.note_on(key, vel);
            return;
        }
        self.voices[self.len] = Voice { key, vel, ..Voice::default() };
        self.len += 1;
        // Premier coup au `tick` qui suit.
    }

    fn note_off(&mut self, key: u8, out: &mut NoteBuffer) {
        let Some(i) = self.voices[..self.len].iter().position(|v| v.key == key) else {
            if std::mem::replace(&mut self.passed[key.min(127) as usize], false) {
                out.note_off(key);
            }
            return;
        };
        // Un nombre fixe de coups va au bout, même relâché.
        if self.hits() == 0 {
            if self.voices[i].sounding {
                out.note_off(key);
            }
            self.remove(i);
        }
    }

    fn advance(&mut self, frames: u32) {
        for v in &mut self.voices[..self.len] {
            v.next.advance(frames);
            v.gate.advance(frames);
        }
    }

    fn tick(&mut self, out: &mut NoteBuffer) {
        let max = self.hits();
        let mut i = 0;
        while i < self.len {
            let v = self.voices[i];
            if v.sounding && v.gate.due() {
                out.note_off(v.key);
                self.voices[i].sounding = false;
            }
            let more = max == 0 || v.hits < max;
            if more && v.next.due() {
                self.strike(i, out);
                continue;
            }
            if !more && !self.voices[i].sounding {
                self.remove(i);
            } else {
                i += 1;
            }
        }
    }

    fn next_event(&self) -> Option<u32> {
        let max = self.hits();
        self.voices[..self.len]
            .iter()
            .filter_map(|v| {
                let next = (max == 0 || v.hits < max).then(|| v.next.wait());
                let gate = v.sounding.then(|| v.gate.wait());
                next.into_iter().chain(gate).min()
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::super::NoteEvent;
    use super::super::testing::{make, offs, ons, play};

    #[test]
    fn held_note_repeats_until_release() {
        let mut fx = make("repeat");
        // 1/32 à 120 BPM : 3 000 échantillons ; durée 50 %.
        let log = play(&mut *fx, &[(0, NoteEvent::on(60, 1.0)), (10_000, NoteEvent::off(60))], 20_000);
        assert_eq!(ons(&log), [(0, 60), (3_000, 60), (6_000, 60), (9_000, 60)]);
        assert_eq!(offs(&log), [(1_500, 60), (4_500, 60), (7_500, 60), (10_000, 60)]);
    }

    #[test]
    fn fixed_count_goes_on_after_release_with_decay() {
        let mut fx = make("repeat");
        fx.set_param(1, 2.0); // 3 coups.
        fx.set_param(3, 0.5);
        let log = play(&mut *fx, &[(0, NoteEvent::on(60, 1.0)), (100, NoteEvent::off(60))], 20_000);
        assert_eq!(ons(&log), [(0, 60), (3_000, 60), (6_000, 60)]);
        assert_eq!(offs(&log).len(), 3);
        let vels: Vec<f32> = log.iter().filter(|(_, e)| e.on).map(|(_, e)| e.vel).collect();
        assert_eq!(vels, [1.0, 0.5, 0.25]);
        assert_eq!(fx.next_event(), None);
    }
}
