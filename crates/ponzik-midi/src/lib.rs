//! Entrées MIDI maison de Ponzik, sans dépendance : on parle directement
//! aux API du système.
//!
//! - Linux : ALSA rawmidi (`libasound`), un thread de lecture par port.
//! - Windows : WinMM (`midiIn*`), rappel système.
//!
//! Les événements arrivent dans un canal ; [`MidiInput::poll`] les vide sans bloquer.
//! Les canaux MIDI sont numérotés de 0 à 15.

use std::fmt;
use std::sync::mpsc::{self, Receiver};

pub mod parser;

#[cfg(target_os = "linux")]
#[path = "alsa.rs"]
mod backend;

#[cfg(windows)]
#[path = "winmm.rs"]
mod backend;

#[cfg(not(any(target_os = "linux", windows)))]
#[path = "unsupported.rs"]
mod backend;

/// Message MIDI reçu. Un NoteOn de vélocité 0 est rapporté comme NoteOff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MidiEvent {
    NoteOn { channel: u8, key: u8, velocity: u8 },
    NoteOff { channel: u8, key: u8, velocity: u8 },
    ControlChange { channel: u8, controller: u8, value: u8 },
    /// `value` dans -8192..=8191, 0 au repos.
    PitchBend { channel: u8, value: i16 },
    ChannelPressure { channel: u8, pressure: u8 },
    PolyPressure { channel: u8, key: u8, pressure: u8 },
    ProgramChange { channel: u8, program: u8 },
    Clock,
    Start,
    Continue,
    Stop,
}

/// Port d'entrée disponible. `id` sert à [`open_input`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub enum MidiError {
    /// Aucun port ne correspond à cet identifiant.
    PortNotFound(String),
    UnsupportedPlatform,
    System(String),
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::PortNotFound(id) => write!(f, "port MIDI introuvable : {id}"),
            MidiError::UnsupportedPlatform => write!(f, "MIDI non supporté sur cette plateforme (Linux et Windows uniquement)"),
            MidiError::System(msg) => write!(f, "erreur MIDI système : {msg}"),
        }
    }
}

impl std::error::Error for MidiError {}

/// Liste les ports d'entrée MIDI (vide s'il n'y a aucun périphérique).
pub fn list_inputs() -> Result<Vec<MidiPortInfo>, MidiError> {
    backend::list()
}

/// Port ouvert ; le détruire ferme le port (et termine son thread de lecture).
pub struct MidiInput {
    name: String,
    rx: Receiver<MidiEvent>,
    _port: backend::Port,
}

/// Ouvre le port d'entrée `id` (tel que renvoyé par [`list_inputs`]).
pub fn open_input(id: &str) -> Result<MidiInput, MidiError> {
    let name = list_inputs()?
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.name)
        .unwrap_or_else(|| id.to_owned());
    let (tx, rx) = mpsc::channel();
    let port = backend::open(id, &name, tx)?;
    Ok(MidiInput { name, rx, _port: port })
}

/// Ouvre tous les ports d'entrée disponibles, en ignorant ceux qui refusent.
pub fn open_all_inputs() -> Vec<MidiInput> {
    list_inputs().unwrap_or_default().iter().filter_map(|p| open_input(&p.id).ok()).collect()
}

impl MidiInput {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Ajoute à `out` les événements reçus depuis le dernier appel, sans bloquer.
    pub fn poll(&self, out: &mut Vec<MidiEvent>) {
        out.extend(self.rx.try_iter());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sans matériel : ne doit ni paniquer ni échouer. `cargo test -p ponzik-midi -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn lists_real_inputs() {
        let ports = list_inputs().expect("list_inputs");
        println!("{} port(s) MIDI : {ports:#?}", ports.len());
        for input in open_all_inputs() {
            println!("ouvert : {}", input.name());
        }
        let err = open_input("hw:99,0,0").err().expect("port inexistant");
        println!("port inexistant : {err}");
    }

    #[test]
    fn bogus_id_is_an_error() {
        assert!(open_input("ponzik-port-inexistant").is_err());
    }
}
