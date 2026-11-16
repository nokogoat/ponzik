//! Effets de notes (MIDI) d'un canal : arpégiateur, accords, gamme, aléatoire,
//! répétition.
//!
//! Ils se placent avant l'instrument, en chaîne (jusqu'à
//! [`crate::rack::NOTE_FX_SLOTS`] par canal) : chaque maillon reçoit les notes
//! du précédent et écrit les siennes dans un [`NoteBuffer`] de taille fixe.
//!
//! Tout se passe « maintenant » : un effet qui veut jouer plus tard (arpège,
//! humanisation, répétitions) garde ses échéances et annonce la prochaine par
//! [`NoteFx::next_event`] ; le moteur découpe alors son bloc pile à cet
//! échantillon et rappelle [`NoteFx::tick`]. Aucune allocation, aucun verrou.

mod arpeggiator;
mod chord;
mod random;
mod repeat;
mod scale;

pub use arpeggiator::Arpeggiator;
pub use chord::Chord;
pub use random::Humanizer;
pub use repeat::Repeat;
pub use scale::ScaleSnap;

use crate::param::Params;

/// Événements que peut contenir un [`NoteBuffer`].
pub const NOTE_BUFFER: usize = 128;

/// Une note qui commence (`on`) ou s'arrête ; `vel` dans [0, 1], ignorée à l'arrêt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEvent {
    pub key: u8,
    pub vel: f32,
    pub on: bool,
}

impl NoteEvent {
    pub const fn on(key: u8, vel: f32) -> Self {
        Self { key, vel, on: true }
    }

    pub const fn off(key: u8) -> Self {
        Self { key, vel: 0.0, on: false }
    }
}

/// File d'événements de taille fixe, sans allocation.
#[derive(Clone, Debug)]
pub struct NoteBuffer {
    events: [NoteEvent; NOTE_BUFFER],
    len: usize,
}

impl Default for NoteBuffer {
    fn default() -> Self {
        Self { events: [NoteEvent::off(0); NOTE_BUFFER], len: 0 }
    }
}

impl NoteBuffer {
    /// `false` (événement perdu) si la file est pleine.
    pub fn push(&mut self, ev: NoteEvent) -> bool {
        if self.len == NOTE_BUFFER {
            return false;
        }
        self.events[self.len] = ev;
        self.len += 1;
        true
    }

    pub fn note_on(&mut self, key: u8, vel: f32) -> bool {
        self.push(NoteEvent::on(key, vel))
    }

    pub fn note_off(&mut self, key: u8) -> bool {
        self.push(NoteEvent::off(key))
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[NoteEvent] {
        &self.events[..self.len]
    }
}

/// Un effet de notes, inséré avant l'instrument d'un canal.
///
/// Les méthodes de jeu sont appelées depuis le thread audio : ni allocation, ni
/// verrou. `set_sample_rate` est appelé par l'interface avant l'envoi au moteur.
///
/// À chaque instant où il se passe quelque chose (début de sous-bloc), le moteur
/// appelle, dans l'ordre : `advance(écoulé)`, puis `note_on` / `note_off` pour
/// chaque note entrante, puis `tick`. Une note relâchée pile sur un pas
/// d'arpège ne le déclenche donc pas.
pub trait NoteFx: Params + Send {
    /// Identifiant stable dans le registre (sauvegarde des projets).
    fn kind(&self) -> &'static str;

    fn set_sample_rate(&mut self, sample_rate: f32);

    /// Tempo courant, pour les cadences synchronisées.
    fn set_tempo(&mut self, _bpm: f32) {}

    /// Oublie tout (notes tenues, échéances) : stop du transport, panique.
    /// L'instrument est coupé à part ; rien n'est émis.
    fn reset(&mut self) {}

    fn note_on(&mut self, key: u8, vel: f32, out: &mut NoteBuffer);

    fn note_off(&mut self, key: u8, out: &mut NoteBuffer);

    /// Avance de `frames` échantillons, sans rien émettre : le moteur découpe ses
    /// blocs pour qu'aucune échéance ([`NoteFx::next_event`]) ne tombe avant la fin.
    fn advance(&mut self, _frames: u32) {}

    /// Émet ce qui est dû à l'instant présent (après les notes entrantes).
    fn tick(&mut self, _out: &mut NoteBuffer) {}

    /// Échantillons avant la prochaine émission spontanée (`0` : maintenant),
    /// `None` s'il n'y en a pas de prévue.
    fn next_event(&self) -> Option<u32> {
        None
    }
}

/// Cadences synchronisées au tempo.
pub(crate) const RATES: &[&str] = &["1/4", "1/8", "1/8 T", "1/16", "1/16 T", "1/32", "1/32 T", "1/64"];
/// Durée de chaque cadence de [`RATES`], en noires.
const RATE_BEATS: [f64; 8] = [1.0, 0.5, 1.0 / 3.0, 0.25, 1.0 / 6.0, 0.125, 1.0 / 12.0, 0.0625];

/// Durée d'un pas de la cadence `rate` (indice dans [`RATES`]), en échantillons.
pub(crate) fn step_samples(rate: usize, bpm: f32, sample_rate: f32) -> f64 {
    let beats = RATE_BEATS[rate.min(RATE_BEATS.len() - 1)];
    beats * 60.0 / bpm.clamp(crate::effects::MIN_BPM, 999.0) as f64 * sample_rate as f64
}

/// Échéance en échantillons, fractionnaire pour que les cadences ne dérivent pas.
/// Elle tombe sur l'échantillon le plus proche.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Countdown(pub f64);

impl Countdown {
    pub fn due(self) -> bool {
        self.0 < 0.5
    }

    /// Plus petit nombre d'échantillons au bout duquel l'échéance est due.
    pub fn wait(self) -> u32 {
        ((self.0 - 0.5).floor() + 1.0).clamp(0.0, u32::MAX as f64) as u32
    }

    pub fn advance(&mut self, frames: u32) {
        self.0 -= frames as f64;
    }
}

/// Générateur pseudo-aléatoire déterministe (xorshift32).
#[derive(Clone, Debug)]
pub(crate) struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniforme dans [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Uniforme dans [0, `n`) ; `n` > 0.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u32() as usize) % n.max(1)
    }
}

/// Note transposée de `semitones`, `None` hors de [0, 127].
pub(crate) fn transpose(key: u8, semitones: i32) -> Option<u8> {
    u8::try_from(key as i32 + semitones).ok().filter(|&k| k <= 127)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub const SR: f32 = 48_000.0;

    /// Effet du registre, prêt à jouer à 48 kHz et 120 BPM.
    pub fn make(kind: &str) -> Box<dyn NoteFx> {
        let mut fx = crate::registry::create_note_fx(kind).unwrap();
        fx.set_sample_rate(SR);
        fx.set_tempo(120.0);
        fx
    }

    /// Joue des événements (échantillon, événement), triés, comme le moteur :
    /// à chaque instant `advance(écoulé)`, les notes entrantes, `tick`. S'arrête
    /// à `until` ; renvoie les (échantillon, événement) émis.
    pub fn play(fx: &mut dyn NoteFx, input: &[(u64, NoteEvent)], until: u64) -> Vec<(u64, NoteEvent)> {
        let mut out = NoteBuffer::default();
        let mut log = Vec::new();
        let (mut now, mut elapsed, mut next_in) = (0, 0, 0);
        loop {
            out.clear();
            fx.advance(elapsed);
            while let Some(&(_, ev)) = input.get(next_in).filter(|(at, _)| *at <= now) {
                if ev.on {
                    fx.note_on(ev.key, ev.vel, &mut out);
                } else {
                    fx.note_off(ev.key, &mut out);
                }
                next_in += 1;
            }
            fx.tick(&mut out);
            log.extend(out.as_slice().iter().map(|&e| (now, e)));
            if now >= until {
                return log;
            }
            let mut next = until;
            if let Some(&(at, _)) = input.get(next_in) {
                next = next.min(at);
            }
            if let Some(w) = fx.next_event() {
                next = next.min(now + w.max(1) as u64);
            }
            elapsed = (next - now) as u32;
            now = next;
        }
    }

    pub fn ons(log: &[(u64, NoteEvent)]) -> Vec<(u64, u8)> {
        log.iter().filter(|(_, e)| e.on).map(|&(t, e)| (t, e.key)).collect()
    }

    pub fn offs(log: &[(u64, NoteEvent)]) -> Vec<(u64, u8)> {
        log.iter().filter(|(_, e)| !e.on).map(|&(t, e)| (t, e.key)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_lands_on_the_nearest_sample() {
        let mut c = Countdown(4_000.4);
        assert_eq!(c.wait(), 4_000);
        c.advance(4_000);
        assert!(c.due());
        assert_eq!(Countdown(0.2).wait(), 0);
        assert_eq!(Countdown(0.5).wait(), 1);
    }

    #[test]
    fn sixteenth_at_120_bpm_is_6000_samples() {
        assert_eq!(step_samples(3, 120.0, 48_000.0), 6_000.0);
        assert_eq!(step_samples(0, 120.0, 48_000.0), 24_000.0);
    }

    #[test]
    fn full_buffer_refuses_events() {
        let mut b = NoteBuffer::default();
        for _ in 0..NOTE_BUFFER {
            assert!(b.note_on(60, 1.0));
        }
        assert!(!b.note_off(60));
        assert_eq!(b.len(), NOTE_BUFFER);
    }
}
