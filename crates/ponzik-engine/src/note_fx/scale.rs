use super::{NoteBuffer, NoteFx};
use crate::effects::Values;
use crate::param::{ParamInfo, Params};

const P_ROOT: u32 = 0;
const P_MODE: u32 = 1;
const P_SNAP: u32 = 2;

const NONE: u8 = u8::MAX;

const ROOTS: &[&str] = &["Do", "Do♯", "Ré", "Mi♭", "Mi", "Fa", "Fa♯", "Sol", "La♭", "La", "Si♭", "Si"];
const MODES: &[&str] = &[
    "Majeur",
    "Mineur",
    "Dorien",
    "Phrygien",
    "Lydien",
    "Mixolydien",
    "Locrien",
    "Mineur harmonique",
    "Mineur mélodique",
    "Pentatonique majeure",
    "Pentatonique mineure",
    "Blues",
];
/// Degrés de chaque mode de [`MODES`], en demi-tons au-dessus de la tonique.
const MODE_STEPS: [&[u8]; 12] = [
    &[0, 2, 4, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 8, 10],
    &[0, 2, 3, 5, 7, 9, 10],
    &[0, 1, 3, 5, 7, 8, 10],
    &[0, 2, 4, 6, 7, 9, 11],
    &[0, 2, 4, 5, 7, 9, 10],
    &[0, 1, 3, 5, 6, 8, 10],
    &[0, 2, 3, 5, 7, 8, 11],
    &[0, 2, 3, 5, 7, 9, 11],
    &[0, 2, 4, 7, 9],
    &[0, 3, 5, 7, 10],
    &[0, 3, 5, 6, 7, 10],
];
const SNAPS: &[&str] = &["Plus proche", "Vers le haut", "Vers le bas", "Filtrer"];

static PARAMS: [ParamInfo; 3] = [
    ParamInfo::choice(P_ROOT, "Tonique", ROOTS, 0).group("Gamme"),
    ParamInfo::choice(P_MODE, "Mode", MODES, 0).group("Gamme"),
    ParamInfo::choice(P_SNAP, "Hors gamme", SNAPS, 0).group("Gamme"),
];

/// Gamme : ramène chaque note sur la gamme choisie (la plus proche, vers le
/// haut, vers le bas) ou laisse tomber celles qui n'en sont pas.
///
/// À égale distance, « Plus proche » descend. La note émise est retenue par
/// note jouée pour l'arrêter même si la gamme change entre-temps.
pub struct ScaleSnap {
    values: Values<3>,
    sounding: [u8; 128],
}

impl Default for ScaleSnap {
    fn default() -> Self {
        Self { values: Values::new(&PARAMS), sounding: [NONE; 128] }
    }
}

impl ScaleSnap {
    fn in_scale(&self, key: i32) -> bool {
        let root = self.values.index(P_ROOT) as i32;
        let steps = MODE_STEPS[self.values.index(P_MODE).min(MODE_STEPS.len() - 1)];
        steps.contains(&((key - root).rem_euclid(12) as u8))
    }

    fn snap(&self, key: u8) -> Option<u8> {
        let key = key as i32;
        let ok = |k: i32| (0..=127).contains(&k) && self.in_scale(k);
        let found = match self.values.index(P_SNAP) {
            1 => (0..12).map(|d| key + d).find(|&k| ok(k)),
            2 => (0..12).map(|d| key - d).find(|&k| ok(k)),
            3 => Some(key).filter(|&k| ok(k)),
            _ => (0..12).flat_map(|d| [key - d, key + d]).find(|&k| ok(k)),
        };
        found.map(|k| k as u8)
    }
}

impl Params for ScaleSnap {
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

impl NoteFx for ScaleSnap {
    fn kind(&self) -> &'static str {
        "scale"
    }

    fn set_sample_rate(&mut self, _sample_rate: f32) {}

    fn reset(&mut self) {
        self.sounding = [NONE; 128];
    }

    fn note_on(&mut self, key: u8, vel: f32, out: &mut NoteBuffer) {
        let key = key.min(127);
        self.note_off(key, out);
        if let Some(k) = self.snap(key) {
            self.sounding[key as usize] = k;
            out.note_on(k, vel);
        }
    }

    fn note_off(&mut self, key: u8, out: &mut NoteBuffer) {
        let k = std::mem::replace(&mut self.sounding[key.min(127) as usize], NONE);
        if k != NONE {
            out.note_off(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::NoteBuffer;
    use super::super::testing::make;

    /// Notes émises pour chaque note de `keys`, avec ces paramètres.
    fn snapped(params: &[(u32, f32)], keys: &[u8]) -> Vec<Option<u8>> {
        let mut fx = make("scale");
        for &(id, v) in params {
            fx.set_param(id, v);
        }
        keys.iter()
            .map(|&k| {
                let mut out = NoteBuffer::default();
                fx.note_on(k, 1.0, &mut out);
                fx.note_off(k, &mut out);
                out.as_slice().first().map(|e| e.key)
            })
            .collect()
    }

    #[test]
    fn snaps_to_c_major() {
        // Do♯ → Do (égalité : vers le bas), Fa♯ → Fa, Si → Si, Mi♭ → Ré.
        assert_eq!(snapped(&[], &[61, 66, 71, 63, 60]), [Some(60), Some(65), Some(71), Some(62), Some(60)]);
        assert_eq!(snapped(&[(2, 1.0)], &[61, 66]), [Some(62), Some(67)]);
        assert_eq!(snapped(&[(2, 2.0)], &[61, 66]), [Some(60), Some(65)]);
        assert_eq!(snapped(&[(2, 3.0)], &[61, 62]), [None, Some(62)]);
    }

    #[test]
    fn root_and_mode() {
        // La mineur pentatonique : la, do, ré, mi, sol.
        let params = [(0, 9.0), (1, 10.0), (2, 3.0)];
        let keys: Vec<u8> = (57..69).collect();
        let kept: Vec<u8> = snapped(&params, &keys).into_iter().flatten().collect();
        assert_eq!(kept, [57, 60, 62, 64, 67]);
    }

    #[test]
    fn note_off_follows_the_note_that_was_played() {
        let mut fx = make("scale");
        let mut out = NoteBuffer::default();
        fx.note_on(61, 1.0, &mut out);
        fx.set_param(0, 1.0); // Do♯ majeur : Do♯ est maintenant dans la gamme.
        fx.note_off(61, &mut out);
        assert_eq!(out.as_slice().iter().map(|e| (e.key, e.on)).collect::<Vec<_>>(), [(60, true), (60, false)]);
    }
}
