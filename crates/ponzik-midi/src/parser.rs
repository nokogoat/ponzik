//! Analyse d'un flux d'octets MIDI 1.0 (tel que le livre un port rawmidi).
//!
//! Gère le statut courant (running status), les octets temps réel glissés au
//! milieu d'un message, les SysEx (ignorés) et les messages système communs
//! (consommés sans être rapportés).

use crate::MidiEvent;

/// Décode un message court complet. `d1`/`d2` sont ignorés s'ils ne servent pas.
/// Renvoie `None` pour ce que Ponzik ne rapporte pas (SysEx, MTC, Active Sensing…).
pub fn decode(status: u8, d1: u8, d2: u8) -> Option<MidiEvent> {
    let channel = status & 0x0F;
    let (d1, d2) = (d1 & 0x7F, d2 & 0x7F);
    Some(match status {
        0x80..=0x8F => MidiEvent::NoteOff { channel, key: d1, velocity: d2 },
        0x90..=0x9F if d2 == 0 => MidiEvent::NoteOff { channel, key: d1, velocity: 0 },
        0x90..=0x9F => MidiEvent::NoteOn { channel, key: d1, velocity: d2 },
        0xA0..=0xAF => MidiEvent::PolyPressure { channel, key: d1, pressure: d2 },
        0xB0..=0xBF => MidiEvent::ControlChange { channel, controller: d1, value: d2 },
        0xC0..=0xCF => MidiEvent::ProgramChange { channel, program: d1 },
        0xD0..=0xDF => MidiEvent::ChannelPressure { channel, pressure: d1 },
        0xE0..=0xEF => MidiEvent::PitchBend { channel, value: (((d2 as i16) << 7) | d1 as i16) - 8192 },
        0xF8 => MidiEvent::Clock,
        0xFA => MidiEvent::Start,
        0xFB => MidiEvent::Continue,
        0xFC => MidiEvent::Stop,
        _ => return None,
    })
}

/// Nombre d'octets de données attendus après un statut (hors temps réel et SysEx).
fn data_len(status: u8) -> usize {
    match status {
        0xC0..=0xDF | 0xF1 | 0xF3 => 1,
        0x80..=0xEF | 0xF2 => 2,
        _ => 0,
    }
}

/// Analyseur à état : on lui pousse les octets dans l'ordre d'arrivée.
#[derive(Clone, Debug, Default)]
pub struct Parser {
    /// Statut en cours (0 = aucun : données orphelines ignorées).
    status: u8,
    data: [u8; 2],
    len: usize,
    in_sysex: bool,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pousse un octet ; renvoie l'événement qu'il complète, le cas échéant.
    pub fn push(&mut self, byte: u8) -> Option<MidiEvent> {
        // Temps réel : peut surgir n'importe où, ne touche pas à l'état.
        if byte >= 0xF8 {
            return decode(byte, 0, 0);
        }
        if byte >= 0x80 {
            self.len = 0;
            // Tout statut (F7 compris) termine un SysEx.
            self.in_sysex = byte == 0xF0;
            self.status = match byte {
                0x80..=0xEF | 0xF1..=0xF3 => byte,
                _ => 0, // F0, F4/F5 (indéfinis), F6 (Tune Request), F7
            };
            return None;
        }
        if self.in_sysex || self.status == 0 {
            return None;
        }
        self.data[self.len] = byte;
        self.len += 1;
        if self.len < data_len(self.status) {
            return None;
        }
        self.len = 0;
        let status = self.status;
        if status >= 0xF0 {
            // Les messages système communs annulent le statut courant.
            self.status = 0;
        }
        decode(status, self.data[0], self.data[1])
    }

    /// Analyse un bloc d'octets et ajoute les événements à `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<MidiEvent>) {
        out.extend(bytes.iter().filter_map(|&b| self.push(b)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MidiEvent::*;

    fn parse(bytes: &[u8]) -> Vec<MidiEvent> {
        let mut out = Vec::new();
        Parser::new().feed(bytes, &mut out);
        out
    }

    #[test]
    fn three_byte_messages() {
        assert_eq!(
            parse(&[0x90, 60, 100, 0x81, 60, 64, 0xB2, 7, 127, 0xA3, 61, 20]),
            [
                NoteOn { channel: 0, key: 60, velocity: 100 },
                NoteOff { channel: 1, key: 60, velocity: 64 },
                ControlChange { channel: 2, controller: 7, value: 127 },
                PolyPressure { channel: 3, key: 61, pressure: 20 },
            ]
        );
    }

    #[test]
    fn two_byte_messages() {
        assert_eq!(
            parse(&[0xC5, 12, 0xDF, 99]),
            [ProgramChange { channel: 5, program: 12 }, ChannelPressure { channel: 15, pressure: 99 }]
        );
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        assert_eq!(parse(&[0x94, 64, 0]), [NoteOff { channel: 4, key: 64, velocity: 0 }]);
    }

    #[test]
    fn pitch_bend_range() {
        assert_eq!(
            parse(&[0xE0, 0, 0, 0xE0, 0, 0x40, 0xE0, 0x7F, 0x7F]),
            [
                PitchBend { channel: 0, value: -8192 },
                PitchBend { channel: 0, value: 0 },
                PitchBend { channel: 0, value: 8191 },
            ]
        );
    }

    #[test]
    fn running_status() {
        assert_eq!(
            parse(&[0x90, 60, 100, 62, 90, 60, 0, 0xC0, 1, 2]),
            [
                NoteOn { channel: 0, key: 60, velocity: 100 },
                NoteOn { channel: 0, key: 62, velocity: 90 },
                NoteOff { channel: 0, key: 60, velocity: 0 },
                ProgramChange { channel: 0, program: 1 },
                ProgramChange { channel: 0, program: 2 },
            ]
        );
    }

    #[test]
    fn real_time_inside_messages() {
        assert_eq!(
            parse(&[0x90, 0xF8, 60, 0xFE, 100, 0xFA, 61, 0xFC, 70]),
            [
                Clock,
                NoteOn { channel: 0, key: 60, velocity: 100 },
                Start,
                Stop,
                NoteOn { channel: 0, key: 61, velocity: 70 },
            ]
        );
        assert_eq!(parse(&[0xFB, 0xF9, 0xFD, 0xFF]), [Continue]);
    }

    #[test]
    fn sysex_is_skipped() {
        assert_eq!(
            parse(&[0xF0, 0x7E, 0x10, 0x01, 0xF8, 0x7F, 0xF7, 0x90, 60, 1]),
            [Clock, NoteOn { channel: 0, key: 60, velocity: 1 }]
        );
        // SysEx sans F7 terminé par un nouveau statut.
        assert_eq!(parse(&[0xF0, 1, 2, 0xB0, 1, 2]), [ControlChange { channel: 0, controller: 1, value: 2 }]);
    }

    #[test]
    fn sysex_cancels_running_status() {
        assert_eq!(parse(&[0x90, 60, 100, 0xF0, 1, 0xF7, 62, 90]), [NoteOn { channel: 0, key: 60, velocity: 100 }]);
    }

    #[test]
    fn system_common_consumed_and_cancels_running_status() {
        assert_eq!(
            parse(&[0x90, 60, 100, 0xF2, 0x10, 0x20, 0xF1, 0x05, 0xF3, 3, 0xF6, 62, 90, 0x80, 60, 0]),
            [NoteOn { channel: 0, key: 60, velocity: 100 }, NoteOff { channel: 0, key: 60, velocity: 0 }]
        );
    }

    #[test]
    fn orphan_data_and_truncated_message() {
        // Données sans statut ignorées ; un statut coupe le message incomplet.
        assert_eq!(parse(&[1, 2, 3, 0x90, 60, 0xB0, 7, 100]), [ControlChange { channel: 0, controller: 7, value: 100 }]);
    }

    #[test]
    fn split_across_feeds() {
        let mut p = Parser::new();
        let mut out = Vec::new();
        p.feed(&[0x90, 60], &mut out);
        assert!(out.is_empty());
        p.feed(&[100, 61], &mut out);
        p.feed(&[50], &mut out);
        assert_eq!(out, [NoteOn { channel: 0, key: 60, velocity: 100 }, NoteOn { channel: 0, key: 61, velocity: 50 }]);
    }

    #[test]
    fn decode_short_messages() {
        assert_eq!(decode(0x9A, 36, 0), Some(NoteOff { channel: 10, key: 36, velocity: 0 }));
        assert_eq!(decode(0xF8, 0, 0), Some(Clock));
        assert_eq!(decode(0xFE, 0, 0), None);
        assert_eq!(decode(0xF0, 0, 0), None);
    }
}
