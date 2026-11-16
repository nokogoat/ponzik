//! External MIDI : au lieu de rendre du son, joue un synthé matériel (ou une
//! autre application) en envoyant les notes du canal sur un port de sortie MIDI.
//!
//! - Notes : note on (vélocité), note off ; transposition ; une note relancée
//!   ou coupée garde le port, le canal et la hauteur de son envoi.
//! - Coupure (stop, panique, changement d'instrument) : note off de chaque note
//!   tenue puis « All Notes Off » (CC 123).
//! - Molette de modulation (CC 1) et pitch bend : paramètres automatisables,
//!   envoyés à chaque changement.
//! - Décalage de latence : positif, les messages partent en avance pour
//!   compenser un appareil lent (dans la limite de l'avance disponible : latence
//!   de sortie audio + latence du mixer ; au-delà ils partent tout de suite).
//!
//! Retour audio : pour entendre et enregistrer le synthé dans Ponzik, brancher
//! sa sortie sur une entrée de la carte son et armer ou écouter une piste sur
//! cette entrée (l'enregistrement et l'écoute existent déjà). Un appariement
//! automatique instrument ↔ entrée reste à faire (crochet : un futur paramètre
//! « Audio return » qui choisirait la source d'écoute de la piste du canal).

use crate::Instrument;
use crate::midi_out::{MidiOutEvent, ShortMessage};
use crate::param::{ParamInfo, Params, Unit};
use ponzik_midi::message::CC_MOD_WHEEL;

const P_PORT: u32 = 0;
const P_CHANNEL: u32 = 1;
const P_TRANSPOSE: u32 = 2;
const P_LATENCY: u32 = 3;
const P_MOD: u32 = 4;
const P_BEND: u32 = 5;

/// Messages gardés entre deux relevés du moteur (un sous-bloc).
const QUEUE: usize = 256;

static PORTS: [&str; 16] = [
    "Port 1", "Port 2", "Port 3", "Port 4", "Port 5", "Port 6", "Port 7", "Port 8", "Port 9", "Port 10", "Port 11",
    "Port 12", "Port 13", "Port 14", "Port 15", "Port 16",
];
static CHANNELS: [&str; 16] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16"];

static PARAMS: [ParamInfo; 6] = [
    ParamInfo::choice(P_PORT, "Port", &PORTS, 0).group("Output"),
    ParamInfo::choice(P_CHANNEL, "Channel", &CHANNELS, 0).group("Output"),
    ParamInfo::new(P_TRANSPOSE, "Transpose", -48.0, 48.0, 0.0, Unit::Semitones).group("Output"),
    ParamInfo::new(P_LATENCY, "Latency offset", -0.1, 0.1, 0.0, Unit::Seconds).group("Output"),
    ParamInfo::new(P_MOD, "Mod wheel", 0.0, 1.0, 0.0, Unit::Percent).group("Controllers"),
    ParamInfo::new(P_BEND, "Pitch bend", -1.0, 1.0, 0.0, Unit::None).group("Controllers"),
];

/// Note envoyée et tenue : où et à quelle hauteur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    port: u8,
    channel: u8,
    key: u8,
}

/// Instrument qui pilote un appareil MIDI externe.
pub struct ExternalMidi {
    port: u8,
    channel: u8,
    transpose: i32,
    latency: f32,
    modulation: f32,
    bend: f32,
    sample_rate: f32,
    /// Note jouée par le moteur → note envoyée.
    held: [Option<Held>; 128],
    queue: [(u8, ShortMessage); QUEUE],
    len: usize,
    /// Quelque chose a été envoyé depuis la dernière coupure.
    active: bool,
}

impl Default for ExternalMidi {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalMidi {
    pub fn new() -> Self {
        Self {
            port: 0,
            channel: 0,
            transpose: 0,
            latency: 0.0,
            modulation: 0.0,
            bend: 0.0,
            sample_rate: 48_000.0,
            held: [None; 128],
            queue: [(0, ShortMessage::default()); QUEUE],
            len: 0,
            active: false,
        }
    }

    fn push(&mut self, port: u8, message: ShortMessage) -> bool {
        if self.len == QUEUE {
            return false;
        }
        self.queue[self.len] = (port, message);
        self.len += 1;
        self.active = true;
        true
    }

    fn mod_value(v: f32) -> u8 {
        (v.clamp(0.0, 1.0) * 127.0).round() as u8
    }

    fn bend_value(v: f32) -> i16 {
        (v.clamp(-1.0, 1.0) * 8191.0).round() as i16
    }
}

impl Params for ExternalMidi {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_PORT => self.port as f32,
            P_CHANNEL => self.channel as f32,
            P_TRANSPOSE => self.transpose as f32,
            P_LATENCY => self.latency,
            P_MOD => self.modulation,
            P_BEND => self.bend,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = self.param_info(id) else { return };
        let v = info.clamp(value);
        match id {
            P_PORT => self.port = v as u8,
            P_CHANNEL => self.channel = v as u8,
            P_TRANSPOSE => self.transpose = v.round() as i32,
            P_LATENCY => self.latency = v,
            P_MOD => {
                let changed = Self::mod_value(v) != Self::mod_value(self.modulation);
                self.modulation = v;
                if changed {
                    self.push(self.port, ShortMessage::control_change(self.channel, CC_MOD_WHEEL, Self::mod_value(v)));
                }
            }
            P_BEND => {
                let changed = Self::bend_value(v) != Self::bend_value(self.bend);
                self.bend = v;
                if changed {
                    self.push(self.port, ShortMessage::pitch_bend(self.channel, Self::bend_value(v)));
                }
            }
            _ => {}
        }
    }
}

impl Instrument for ExternalMidi {
    fn kind(&self) -> &'static str {
        "midi_out"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let Some(slot) = self.held.get(note as usize) else { return };
        // Note relancée : on coupe d'abord celle qui sonne.
        if let Some(h) = *slot
            && self.push(h.port, ShortMessage::note_off(h.channel, h.key, 0))
        {
            self.held[note as usize] = None;
        }
        let key = note as i32 + self.transpose;
        if !(0..=127).contains(&key) {
            return;
        }
        let held = Held { port: self.port, channel: self.channel, key: key as u8 };
        let vel = (velocity.clamp(0.0, 1.0) * 127.0).round() as u8;
        if self.push(held.port, ShortMessage::note_on(held.channel, held.key, vel)) {
            self.held[note as usize] = Some(held);
        }
    }

    fn note_off(&mut self, note: u8) {
        let Some(Some(h)) = self.held.get(note as usize).copied() else { return };
        // File pleine : la note reste tenue, la prochaine coupure la relâchera.
        if self.push(h.port, ShortMessage::note_off(h.channel, h.key, 0)) {
            self.held[note as usize] = None;
        }
    }

    fn all_notes_off(&mut self) {
        if !self.active && self.held.iter().all(Option::is_none) {
            return;
        }
        // Coupure : la file en attente n'a plus de sens, sauf ses note off.
        let pending = self.len;
        self.len = 0;
        for i in 0..pending {
            let (port, m) = self.queue[i];
            if m.status() & 0xF0 == 0x80 {
                self.queue[self.len] = (port, m);
                self.len += 1;
            }
        }
        let mut channels: [(u8, u8); 4] = [(self.port, self.channel); 4];
        let mut count = 1;
        for note in 0..128 {
            if let Some(h) = self.held[note].take() {
                self.push(h.port, ShortMessage::note_off(h.channel, h.key, 0));
                if !channels[..count].contains(&(h.port, h.channel)) && count < channels.len() {
                    channels[count] = (h.port, h.channel);
                    count += 1;
                }
            }
        }
        for &(port, channel) in &channels[..count] {
            self.push(port, ShortMessage::all_notes_off(channel));
        }
        self.active = false;
    }

    fn render(&mut self, _left: &mut [f32], _right: &mut [f32]) {}

    fn drain_midi(&mut self, out: &mut dyn FnMut(MidiOutEvent)) {
        let offset = -(self.latency * self.sample_rate).round() as i32;
        for &(port, message) in &self.queue[..self.len] {
            out(MidiOutEvent { port, offset, message });
        }
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drained(inst: &mut ExternalMidi) -> Vec<(u8, i32, Vec<u8>)> {
        let mut v = Vec::new();
        inst.drain_midi(&mut |e| v.push((e.port, e.offset, e.message.bytes().to_vec())));
        v
    }

    #[test]
    fn notes_follow_port_channel_and_transpose() {
        let mut m = ExternalMidi::new();
        m.set_sample_rate(48_000.0);
        m.set_param(P_PORT, 2.0);
        m.set_param(P_CHANNEL, 9.0);
        m.set_param(P_TRANSPOSE, 12.0);
        m.set_param(P_LATENCY, 0.005);
        m.note_on(60, 1.0);
        // Changer de canal ne doit pas perdre le note off.
        m.set_param(P_CHANNEL, 0.0);
        m.note_off(60);
        m.note_on(127, 0.5);
        assert_eq!(
            drained(&mut m),
            [(2, -240, vec![0x99, 72, 127]), (2, -240, vec![0x89, 72, 0])],
            "127 + 12 is out of range"
        );
        assert!(drained(&mut m).is_empty());
    }

    #[test]
    fn all_notes_off_releases_held_notes_once() {
        let mut m = ExternalMidi::new();
        m.all_notes_off();
        assert!(drained(&mut m).is_empty(), "nothing played, nothing sent");
        m.note_on(60, 0.8);
        m.note_on(64, 0.8);
        drained(&mut m);
        m.all_notes_off();
        let sent: Vec<Vec<u8>> = drained(&mut m).into_iter().map(|e| e.2).collect();
        assert_eq!(sent, [vec![0x80, 60, 0], vec![0x80, 64, 0], vec![0xB0, 123, 0]]);
        m.all_notes_off();
        assert!(drained(&mut m).is_empty());
        m.note_off(60);
        assert!(drained(&mut m).is_empty());
    }

    #[test]
    fn retrigger_and_controllers() {
        let mut m = ExternalMidi::new();
        m.note_on(60, 0.5);
        m.note_on(60, 0.5);
        m.set_param(P_MOD, 1.0);
        m.set_param(P_MOD, 0.999);
        m.set_param(P_BEND, -1.0);
        let sent: Vec<Vec<u8>> = drained(&mut m).into_iter().map(|e| e.2).collect();
        assert_eq!(
            sent,
            [vec![0x90, 60, 64], vec![0x80, 60, 0], vec![0x90, 60, 64], vec![0xB0, 1, 127], vec![0xE0, 1, 0]]
        );
    }
}
