//! Encodage des messages MIDI 1.0 courts (au plus trois octets) envoyés vers
//! les ports de sortie. Pas de statut courant (running status) : chaque
//! message porte son octet de statut, plus robuste si un octet se perd.
//! Les canaux sont numérotés de 0 à 15 ; les valeurs hors bornes sont rognées.

use crate::MidiEvent;

/// Horloge : 24 impulsions par noire.
pub const CLOCK: u8 = 0xF8;
/// Départ depuis le début du morceau.
pub const START: u8 = 0xFA;
/// Reprise depuis la dernière position (Song Position Pointer).
pub const CONTINUE: u8 = 0xFB;
pub const STOP: u8 = 0xFC;
/// Impulsions d'horloge par noire.
pub const CLOCKS_PER_QUARTER: u32 = 24;
/// Impulsions d'horloge par « temps MIDI » (double croche) du Song Position Pointer.
pub const CLOCKS_PER_SIXTEENTH: u32 = 6;
/// Plus grande position exprimable par le Song Position Pointer (14 bits).
pub const MAX_SONG_POSITION: u16 = 0x3FFF;
/// Contrôleur « All Notes Off ».
pub const CC_ALL_NOTES_OFF: u8 = 123;
/// Contrôleur de la pédale de sustain.
pub const CC_SUSTAIN: u8 = 64;
pub const CC_MOD_WHEEL: u8 = 1;

/// Message MIDI court, prêt à écrire sur un port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ShortMessage {
    data: [u8; 3],
    len: u8,
}

impl ShortMessage {
    const fn new(data: [u8; 3], len: u8) -> Self {
        Self { data, len }
    }

    /// Octets à transmettre, statut en tête.
    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    pub fn status(&self) -> u8 {
        self.data[0]
    }

    /// Message temps réel d'un octet ([`CLOCK`], [`START`], [`CONTINUE`], [`STOP`]…).
    pub const fn realtime(status: u8) -> Self {
        Self::new([status, 0, 0], 1)
    }

    /// `velocity` dans 1..=127 : une vélocité nulle signifierait « note off ».
    pub fn note_on(channel: u8, key: u8, velocity: u8) -> Self {
        Self::new([0x90 | (channel & 0x0F), key & 0x7F, velocity.clamp(1, 127)], 3)
    }

    pub fn note_off(channel: u8, key: u8, velocity: u8) -> Self {
        Self::new([0x80 | (channel & 0x0F), key & 0x7F, velocity & 0x7F], 3)
    }

    pub fn control_change(channel: u8, controller: u8, value: u8) -> Self {
        Self::new([0xB0 | (channel & 0x0F), controller & 0x7F, value & 0x7F], 3)
    }

    pub fn program_change(channel: u8, program: u8) -> Self {
        Self::new([0xC0 | (channel & 0x0F), program & 0x7F, 0], 2)
    }

    pub fn channel_pressure(channel: u8, pressure: u8) -> Self {
        Self::new([0xD0 | (channel & 0x0F), pressure & 0x7F, 0], 2)
    }

    pub fn poly_pressure(channel: u8, key: u8, pressure: u8) -> Self {
        Self::new([0xA0 | (channel & 0x0F), key & 0x7F, pressure & 0x7F], 3)
    }

    /// `value` dans -8192..=8191, 0 au repos ; poids faible d'abord.
    pub fn pitch_bend(channel: u8, value: i16) -> Self {
        let v = (value.clamp(-8192, 8191) + 8192) as u16;
        Self::new([0xE0 | (channel & 0x0F), (v & 0x7F) as u8, (v >> 7) as u8], 3)
    }

    /// « All Notes Off » (CC 123) : coupe les notes tenues du canal.
    pub fn all_notes_off(channel: u8) -> Self {
        Self::control_change(channel, CC_ALL_NOTES_OFF, 0)
    }

    /// Song Position Pointer : position en doubles croches depuis le début
    /// (6 impulsions d'horloge chacune), rognée à 14 bits.
    pub fn song_position(sixteenths: u32) -> Self {
        let v = sixteenths.min(MAX_SONG_POSITION as u32) as u16;
        Self::new([0xF2, (v & 0x7F) as u8, (v >> 7) as u8], 3)
    }

    /// Vrai pour un message de canal (note, contrôleur, pitch bend…).
    pub fn is_channel(&self) -> bool {
        (0x80..0xF0).contains(&self.data[0])
    }

    /// Canal (0 à 15) d'un message de canal.
    pub fn channel(&self) -> Option<u8> {
        self.is_channel().then_some(self.data[0] & 0x0F)
    }
}

impl From<MidiEvent> for ShortMessage {
    fn from(event: MidiEvent) -> Self {
        match event {
            MidiEvent::NoteOn { channel, key, velocity: 0 } => Self::note_off(channel, key, 0),
            MidiEvent::NoteOn { channel, key, velocity } => Self::note_on(channel, key, velocity),
            MidiEvent::NoteOff { channel, key, velocity } => Self::note_off(channel, key, velocity),
            MidiEvent::ControlChange { channel, controller, value } => Self::control_change(channel, controller, value),
            MidiEvent::PitchBend { channel, value } => Self::pitch_bend(channel, value),
            MidiEvent::ChannelPressure { channel, pressure } => Self::channel_pressure(channel, pressure),
            MidiEvent::PolyPressure { channel, key, pressure } => Self::poly_pressure(channel, key, pressure),
            MidiEvent::ProgramChange { channel, program } => Self::program_change(channel, program),
            MidiEvent::Clock => Self::realtime(CLOCK),
            MidiEvent::Start => Self::realtime(START),
            MidiEvent::Continue => Self::realtime(CONTINUE),
            MidiEvent::Stop => Self::realtime(STOP),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn channel_messages_encode_to_spec() {
        assert_eq!(ShortMessage::note_on(0, 60, 100).bytes(), [0x90, 60, 100]);
        assert_eq!(ShortMessage::note_on(15, 200, 0).bytes(), [0x9F, 72, 1]);
        assert_eq!(ShortMessage::note_off(9, 36, 64).bytes(), [0x89, 36, 64]);
        assert_eq!(ShortMessage::control_change(2, 1, 127).bytes(), [0xB2, 1, 127]);
        assert_eq!(ShortMessage::all_notes_off(3).bytes(), [0xB3, 123, 0]);
        assert_eq!(ShortMessage::program_change(1, 5).bytes(), [0xC1, 5]);
        assert_eq!(ShortMessage::channel_pressure(0, 70).bytes(), [0xD0, 70]);
        assert_eq!(ShortMessage::note_on(4, 1, 1).channel(), Some(4));
        assert_eq!(ShortMessage::realtime(CLOCK).channel(), None);
    }

    #[test]
    fn pitch_bend_is_14_bit_lsb_first() {
        assert_eq!(ShortMessage::pitch_bend(0, 0).bytes(), [0xE0, 0x00, 0x40]);
        assert_eq!(ShortMessage::pitch_bend(0, -8192).bytes(), [0xE0, 0x00, 0x00]);
        assert_eq!(ShortMessage::pitch_bend(0, 8191).bytes(), [0xE0, 0x7F, 0x7F]);
        assert_eq!(ShortMessage::pitch_bend(1, i16::MAX).bytes(), [0xE1, 0x7F, 0x7F]);
    }

    #[test]
    fn transport_and_song_position() {
        assert_eq!(ShortMessage::realtime(CLOCK).bytes(), [0xF8]);
        assert_eq!(ShortMessage::realtime(START).bytes(), [0xFA]);
        assert_eq!(ShortMessage::realtime(CONTINUE).bytes(), [0xFB]);
        assert_eq!(ShortMessage::realtime(STOP).bytes(), [0xFC]);
        assert_eq!(ShortMessage::song_position(0).bytes(), [0xF2, 0, 0]);
        // Mesure 3 en 4/4 : 2 mesures × 16 doubles croches.
        assert_eq!(ShortMessage::song_position(32).bytes(), [0xF2, 32, 0]);
        assert_eq!(ShortMessage::song_position(300).bytes(), [0xF2, 44, 2]);
        assert_eq!(ShortMessage::song_position(1 << 20).bytes(), [0xF2, 0x7F, 0x7F]);
    }

    #[test]
    fn encoding_round_trips_through_the_parser() {
        let events = [
            MidiEvent::NoteOn { channel: 3, key: 64, velocity: 90 },
            MidiEvent::NoteOff { channel: 3, key: 64, velocity: 0 },
            MidiEvent::ControlChange { channel: 0, controller: 74, value: 12 },
            MidiEvent::PitchBend { channel: 5, value: -1234 },
            MidiEvent::PitchBend { channel: 5, value: 8191 },
            MidiEvent::ChannelPressure { channel: 1, pressure: 33 },
            MidiEvent::PolyPressure { channel: 2, key: 10, pressure: 20 },
            MidiEvent::ProgramChange { channel: 15, program: 99 },
            MidiEvent::Clock,
            MidiEvent::Start,
            MidiEvent::Continue,
            MidiEvent::Stop,
        ];
        let mut parser = Parser::new();
        let mut out = Vec::new();
        for e in events {
            parser.feed(ShortMessage::from(e).bytes(), &mut out);
        }
        assert_eq!(out, events);
    }
}
