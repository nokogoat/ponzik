use super::{Countdown, NoteBuffer, NoteFx, RATES, Rng, step_samples};
use crate::effects::{DEFAULT_SR, Values};
use crate::param::{ParamInfo, Params, Unit};

const P_RATE: u32 = 0;
const P_MODE: u32 = 1;
const P_OCTAVES: u32 = 2;
const P_GATE: u32 = 3;

/// Notes tenues gardées ; au-delà, les nouvelles sont ignorées.
const MAX_HELD: usize = 32;

const MODES: &[&str] = &["Montant", "Descendant", "Montant-descendant", "Aléatoire", "Ordre joué"];
const OCTAVES: &[&str] = &["1", "2", "3", "4"];

static PARAMS: [ParamInfo; 4] = [
    ParamInfo::choice(P_RATE, "Cadence", RATES, 3).group("Rythme"),
    ParamInfo::choice(P_MODE, "Mode", MODES, 0).group("Motif"),
    ParamInfo::choice(P_OCTAVES, "Octaves", OCTAVES, 0).group("Motif"),
    ParamInfo::new(P_GATE, "Durée", 0.05, 1.0, 0.5, Unit::Percent).group("Rythme"),
];

/// Arpégiateur synchronisé : joue une à une les notes tenues, sur une à quatre
/// octaves, à la cadence choisie.
///
/// Le motif démarre à la première note enfoncée (pas de calage sur la mesure) :
/// des notes posées sur la grille donnent un arpège sur la grille. Durée = part
/// du pas pendant laquelle chaque note sonne.
pub struct Arpeggiator {
    values: Values<4>,
    sample_rate: f32,
    bpm: f32,
    /// Notes tenues dans l'ordre où elles ont été jouées : (note, vélocité).
    held: [(u8, f32); MAX_HELD],
    held_len: usize,
    /// Rang du prochain pas dans le motif.
    step: usize,
    next: Countdown,
    gate: Countdown,
    sounding: Option<u8>,
    rng: Rng,
}

impl Default for Arpeggiator {
    fn default() -> Self {
        Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            bpm: 120.0,
            held: [(0, 0.0); MAX_HELD],
            held_len: 0,
            step: 0,
            next: Countdown::default(),
            gate: Countdown::default(),
            sounding: None,
            rng: Rng::new(0x5EED_A4B1),
        }
    }
}

impl Arpeggiator {
    fn step_len(&self) -> f64 {
        step_samples(self.values.index(P_RATE), self.bpm, self.sample_rate)
    }

    /// Note et vélocité du pas `step`.
    fn pick(&mut self, step: usize) -> (u8, f32) {
        let held = &self.held[..self.held_len];
        let n = held.len();
        let octaves = self.values.index(P_OCTAVES) + 1;
        let total = n * octaves;
        let mut sorted = [(0u8, 0f32); MAX_HELD];
        sorted[..n].copy_from_slice(held);
        sorted[..n].sort_unstable_by_key(|&(k, _)| k);
        let mode = self.values.index(P_MODE);
        let index = match mode {
            1 => total - 1 - step % total,
            2 if total > 1 => {
                let period = 2 * total - 2;
                let p = step % period;
                if p < total { p } else { period - p }
            }
            3 => self.rng.below(total),
            _ => step % total,
        };
        let source = if mode == 4 { held } else { &sorted[..n] };
        let (key, vel) = source[index % n];
        let mut key = key as usize + 12 * (index / n);
        while key > 127 {
            key -= 12;
        }
        (key as u8, vel)
    }
}

impl Params for Arpeggiator {
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

impl NoteFx for Arpeggiator {
    fn kind(&self) -> &'static str {
        "arpeggiator"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.reset();
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm;
    }

    fn reset(&mut self) {
        self.held_len = 0;
        self.step = 0;
        self.sounding = None;
    }

    fn note_on(&mut self, key: u8, vel: f32, _out: &mut NoteBuffer) {
        if let Some(h) = self.held[..self.held_len].iter_mut().find(|h| h.0 == key) {
            h.1 = vel;
            return;
        }
        if self.held_len == MAX_HELD {
            return;
        }
        if self.held_len == 0 {
            // Premier pas tout de suite, émis au `tick` qui suit les entrées :
            // un accord plaqué est entièrement tenu avant le choix de la note.
            self.step = 0;
            self.next = Countdown(0.0);
        }
        self.held[self.held_len] = (key, vel);
        self.held_len += 1;
    }

    fn note_off(&mut self, key: u8, out: &mut NoteBuffer) {
        let Some(i) = self.held[..self.held_len].iter().position(|h| h.0 == key) else { return };
        self.held.copy_within(i + 1..self.held_len, i);
        self.held_len -= 1;
        if self.held_len == 0
            && let Some(k) = self.sounding.take()
        {
            out.note_off(k);
        }
    }

    fn advance(&mut self, frames: u32) {
        self.next.advance(frames);
        self.gate.advance(frames);
    }

    fn tick(&mut self, out: &mut NoteBuffer) {
        if self.held_len == 0 {
            return;
        }
        loop {
            if self.sounding.is_some() && self.gate.due() {
                out.note_off(self.sounding.take().unwrap_or_default());
            }
            if !self.next.due() {
                return;
            }
            if let Some(k) = self.sounding.take() {
                out.note_off(k);
            }
            let (key, vel) = self.pick(self.step);
            out.note_on(key, vel);
            self.sounding = Some(key);
            let len = self.step_len();
            self.gate = Countdown(self.next.0 + len * self.values.get(P_GATE) as f64);
            self.next.0 += len;
            self.step = self.step.wrapping_add(1);
        }
    }

    fn next_event(&self) -> Option<u32> {
        if self.held_len == 0 {
            return None;
        }
        let next = self.next.wait();
        Some(if self.sounding.is_some() { next.min(self.gate.wait()) } else { next })
    }
}

#[cfg(test)]
mod tests {
    use super::super::NoteEvent;
    use super::super::testing::{make, offs, ons, play};

    fn chord(keys: &[u8], at: u64, until: u64) -> Vec<(u64, NoteEvent)> {
        let mut v: Vec<_> = keys.iter().map(|&k| (at, NoteEvent::on(k, 0.8))).collect();
        v.extend(keys.iter().map(|&k| (until, NoteEvent::off(k))));
        v
    }

    #[test]
    fn up_sixteenths_at_120_bpm() {
        let mut fx = make("arpeggiator");
        // Jouées dans le désordre : le mode montant trie.
        let log = play(&mut *fx, &chord(&[67, 60, 64], 0, 30_000), 40_000);
        assert_eq!(ons(&log), [(0, 60), (6_000, 64), (12_000, 67), (18_000, 60), (24_000, 64)]);
        // Durée 50 % : chaque note s'arrête à mi-pas ; la dernière au relâchement.
        assert_eq!(offs(&log), [(3_000, 60), (9_000, 64), (15_000, 67), (21_000, 60), (27_000, 64)]);
    }

    #[test]
    fn modes_and_octaves() {
        let seq = |mode: f32, octaves: f32| {
            let mut fx = make("arpeggiator");
            fx.set_param(1, mode);
            fx.set_param(2, octaves);
            fx.set_param(0, 5.0); // 1/32 : 3 000 échantillons.
            let log = play(&mut *fx, &chord(&[64, 60], 0, 3_000 * 8), 3_000 * 8);
            ons(&log).into_iter().map(|(_, k)| k).collect::<Vec<_>>()
        };
        assert_eq!(seq(1.0, 0.0), [64, 60, 64, 60, 64, 60, 64, 60]);
        assert_eq!(seq(0.0, 1.0), [60, 64, 72, 76, 60, 64, 72, 76]);
        assert_eq!(seq(2.0, 1.0), [60, 64, 72, 76, 72, 64, 60, 64]);
        assert_eq!(seq(4.0, 0.0), [64, 60, 64, 60, 64, 60, 64, 60]);
        assert!(seq(3.0, 1.0).iter().all(|k| [60, 64, 72, 76].contains(k)));
    }

    #[test]
    fn full_gate_is_legato_and_triplets_do_not_drift() {
        let mut fx = make("arpeggiator");
        fx.set_param(0, 4.0); // 1/16 T : 4 000 échantillons.
        fx.set_param(3, 1.0);
        let log = play(&mut *fx, &chord(&[60], 0, 4_000 * 30), 4_000 * 30);
        let on = ons(&log);
        assert_eq!(on.len(), 30);
        assert!(on.iter().enumerate().all(|(i, &(t, _))| t == i as u64 * 4_000));
        // Chaque note s'arrête au moment où la suivante commence, juste avant.
        let first_off = log.iter().position(|(_, e)| !e.on).unwrap();
        assert_eq!(log[first_off].0, 4_000);
        assert!(log[first_off + 1].1.on);
    }
}
