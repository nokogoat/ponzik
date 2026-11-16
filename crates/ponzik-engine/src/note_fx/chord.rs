use super::{NoteBuffer, NoteFx, transpose};
use crate::effects::Values;
use crate::param::{ParamInfo, Params, Unit};

const P_PRESET: u32 = 0;
const P_INTERVALS: [u32; 4] = [1, 2, 3, 4];
const P_VELOCITY: u32 = 5;

/// Note jouée plus quatre ajoutées au plus.
const MAX_NOTES: usize = 5;
const NONE: u8 = u8::MAX;

const PRESETS: &[&str] = &[
    "Free",
    "Major",
    "Minor",
    "Fifth",
    "Sus2",
    "Sus4",
    "Major 7",
    "Minor 7",
    "Dominant 7",
    "Diminished",
    "Augmented",
    "Octave",
    "Major 9",
    "Minor 9",
];
/// Intervalles ajoutés par chaque préréglage (0 : aucun) ; « Libre » prend les paramètres.
const PRESET_INTERVALS: [[i32; 4]; 14] = [
    [0, 0, 0, 0],
    [4, 7, 0, 0],
    [3, 7, 0, 0],
    [7, 0, 0, 0],
    [2, 7, 0, 0],
    [5, 7, 0, 0],
    [4, 7, 11, 0],
    [3, 7, 10, 0],
    [4, 7, 10, 0],
    [3, 6, 0, 0],
    [4, 8, 0, 0],
    [12, 0, 0, 0],
    [4, 7, 11, 14],
    [3, 7, 10, 14],
];

static PARAMS: [ParamInfo; 6] = [
    ParamInfo::choice(P_PRESET, "Chord", PRESETS, 1).group("Chord"),
    ParamInfo::new(P_INTERVALS[0], "Interval 1", -24.0, 24.0, 0.0, Unit::Semitones).group("Free"),
    ParamInfo::new(P_INTERVALS[1], "Interval 2", -24.0, 24.0, 0.0, Unit::Semitones).group("Free"),
    ParamInfo::new(P_INTERVALS[2], "Interval 3", -24.0, 24.0, 0.0, Unit::Semitones).group("Free"),
    ParamInfo::new(P_INTERVALS[3], "Interval 4", -24.0, 24.0, 0.0, Unit::Semitones).group("Free"),
    ParamInfo::new(P_VELOCITY, "Added Velocity", 0.0, 1.0, 1.0, Unit::Percent).group("Chord"),
];

/// Accords : chaque note jouée en ajoute jusqu'à quatre, d'après un préréglage
/// ou, en « Libre », d'après les quatre intervalles (0 : rien).
///
/// Les notes ajoutées sont retenues par note jouée : changer d'accord pendant
/// qu'elle est tenue n'empêche pas de bien les arrêter.
pub struct Chord {
    values: Values<6>,
    /// Notes émises par note jouée (`NONE` : case vide).
    sounding: [[u8; MAX_NOTES]; 128],
}

impl Default for Chord {
    fn default() -> Self {
        Self { values: Values::new(&PARAMS), sounding: [[NONE; MAX_NOTES]; 128] }
    }
}

impl Chord {
    fn intervals(&self) -> [i32; 4] {
        match self.values.index(P_PRESET) {
            0 => P_INTERVALS.map(|id| self.values.get(id).round() as i32),
            p => PRESET_INTERVALS[p.min(PRESET_INTERVALS.len() - 1)],
        }
    }

    fn release(&mut self, key: u8, out: &mut NoteBuffer) {
        for k in std::mem::replace(&mut self.sounding[key as usize], [NONE; MAX_NOTES]) {
            if k != NONE {
                out.note_off(k);
            }
        }
    }
}

impl Params for Chord {
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

impl NoteFx for Chord {
    fn kind(&self) -> &'static str {
        "chord"
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {}

    fn reset(&mut self) {
        self.sounding = [[NONE; MAX_NOTES]; 128];
    }

    fn note_on(&mut self, key: u8, vel: f32, out: &mut NoteBuffer) {
        let key = key.min(127);
        self.release(key, out);
        let mut notes = [NONE; MAX_NOTES];
        notes[0] = key;
        out.note_on(key, vel);
        let added_vel = vel * self.values.get(P_VELOCITY);
        let mut n = 1;
        for semitones in self.intervals() {
            if semitones == 0 {
                continue;
            }
            let Some(k) = transpose(key, semitones) else { continue };
            if notes[..n].contains(&k) {
                continue;
            }
            notes[n] = k;
            n += 1;
            out.note_on(k, added_vel);
        }
        self.sounding[key as usize] = notes;
    }

    fn note_off(&mut self, key: u8, out: &mut NoteBuffer) {
        self.release(key.min(127), out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::NoteBuffer;
    use super::super::testing::make;

    fn keys(out: &NoteBuffer, on: bool) -> Vec<u8> {
        out.as_slice().iter().filter(|e| e.on == on).map(|e| e.key).collect()
    }

    #[test]
    fn preset_adds_intervals_and_releases_them() {
        let mut fx = make("chord");
        let mut out = NoteBuffer::default();
        fx.note_on(60, 1.0, &mut out);
        assert_eq!(keys(&out, true), [60, 64, 67]);
        // Changer d'accord pendant la tenue : on arrête bien ce qui a été joué.
        fx.set_param(0, 2.0);
        out.clear();
        fx.note_off(60, &mut out);
        assert_eq!(keys(&out, false), [60, 64, 67]);
    }

    #[test]
    fn free_intervals_skip_zero_duplicates_and_out_of_range() {
        let mut fx = make("chord");
        fx.set_param(0, 0.0);
        fx.set_param(1, 12.0);
        fx.set_param(2, -5.0);
        fx.set_param(3, 12.0);
        fx.set_param(5, 0.5);
        let mut out = NoteBuffer::default();
        fx.note_on(120, 0.8, &mut out);
        assert_eq!(keys(&out, true), [120, 115]);
        assert_eq!(out.as_slice()[1].vel, 0.4);
    }
}
