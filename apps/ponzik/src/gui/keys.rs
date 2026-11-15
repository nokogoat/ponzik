//! Clavier d'ordinateur joué comme un piano, et enregistrement des notes jouées.
//!
//! Disposition par position physique (identique en AZERTY et en QWERTY), comme
//! dans FL Studio : rangée du bas = octave de base, rangée du haut = octave au-dessus,
//! les dièses sur les rangées intermédiaires.

use ponzik_engine::{Command, Note, PlayMode, TICKS_PER_STEP};

use super::Input;
use super::app::App;

/// Code physique → demi-tons au-dessus de l'octave de base.
fn semitone(code: u16) -> Option<u8> {
    Some(match code {
        // Rangée du bas : W/Z X C V B N M , ; :
        44 => 0,
        45 => 2,
        46 => 4,
        47 => 5,
        48 => 7,
        49 => 9,
        50 => 11,
        51 => 12,
        52 => 14,
        53 => 16,
        // Rangée du milieu : dièses de l'octave de base.
        31 => 1,
        32 => 3,
        34 => 6,
        35 => 8,
        36 => 10,
        38 => 13,
        39 => 15,
        // Rangée du haut : A/Q Z/W E R T Y U I O P
        16 => 12,
        17 => 14,
        18 => 16,
        19 => 17,
        20 => 19,
        21 => 21,
        22 => 23,
        23 => 24,
        24 => 26,
        25 => 28,
        // Rangée des chiffres : dièses de l'octave du haut.
        3 => 13,
        4 => 15,
        6 => 18,
        7 => 20,
        8 => 22,
        10 => 25,
        11 => 27,
        _ => return None,
    })
}

const OCTAVE_DOWN: u16 = 12;
const OCTAVE_UP: u16 = 13;

/// Note tenue au clavier : canal, note, début en ticks dans le pattern.
#[derive(Clone, Copy, Debug)]
pub struct HeldNote {
    pub code: u16,
    pub channel: usize,
    pub key: u8,
    pub vel: f32,
    pub start: Option<f64>,
}

impl App {
    /// Notes jouées au clavier ou au contrôleur MIDI : écoute et, si l'enregistrement
    /// est armé pendant la lecture, écriture dans le pattern courant.
    pub fn live_note_on(&mut self, code: u16, key: u8, vel: f32) {
        let Some(ch) = self.selected_channel().or_else(|| (!self.project.channels.is_empty()).then_some(0)) else {
            return;
        };
        self.send(Command::NoteOn { channel: ch, note: key, velocity: vel });
        let recording = self.record && self.playing && matches!(self.project.mode, PlayMode::Pattern(_));
        let start = recording.then(|| self.cycle_position());
        self.held.push(HeldNote { code, channel: ch, key, vel, start });
    }

    pub fn live_note_off(&mut self, code: u16) {
        let Some(i) = self.held.iter().position(|h| h.code == code) else { return };
        let h = self.held.remove(i);
        self.send(Command::NoteOff { channel: h.channel, note: h.key });
        let Some(start) = h.start else { return };
        let pat = self.project.current_pattern;
        let len_pat = self.project.patterns[pat].len.max(1) as f64;
        let mut len = self.cycle_position() - start;
        if len <= 0.0 {
            len += len_pat;
        }
        // Début quantifié à la double-croche, durée au moins d'un demi-pas.
        let step = TICKS_PER_STEP as f64;
        let q = ((start / step).round() * step) % len_pat;
        let note = Note { start: q as u32, len: (len.round() as u32).max(TICKS_PER_STEP / 2), key: h.key, vel: h.vel };
        if h.channel < self.project.channels.len() {
            self.project.add_note(pat, h.channel, note);
            self.commit("Enregistrer des notes");
        }
    }

    pub(super) fn musical_keyboard(&mut self, input: &Input) {
        if self.popup.is_some() || input.mods.ctrl || input.mods.alt {
            return;
        }
        let events = &input.physical;
        let mut i = 0;
        while i < events.len() {
            let (code, down) = events[i];
            // Répétition automatique X11 : relâchement suivi d'un appui de la même touche.
            if !down && events.get(i + 1) == Some(&(code, true)) {
                i += 2;
                continue;
            }
            i += 1;
            match (code, down) {
                (OCTAVE_DOWN, true) => {
                    self.keyboard_octave = (self.keyboard_octave - 1).max(0);
                    self.toast(&format!("Clavier : octave {}", self.keyboard_octave));
                }
                (OCTAVE_UP, true) => {
                    self.keyboard_octave = (self.keyboard_octave + 1).min(8);
                    self.toast(&format!("Clavier : octave {}", self.keyboard_octave));
                }
                (code, true) => {
                    if self.held.iter().any(|h| h.code == code) {
                        continue;
                    }
                    if let Some(st) = semitone(code) {
                        let key = ((self.keyboard_octave + 1) * 12 + st as i32).clamp(0, 127) as u8;
                        self.live_note_on(code, key, 0.8);
                    }
                }
                (code, false) => self.live_note_off(code),
            }
        }
    }

    /// Relâche tout (perte du focus, arrêt).
    pub fn release_all_held(&mut self) {
        let codes: Vec<u16> = self.held.iter().map(|h| h.code).collect();
        for code in codes {
            self.live_note_off(code);
        }
    }
}
