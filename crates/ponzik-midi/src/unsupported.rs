use std::sync::mpsc::Sender;

use crate::{MidiError, MidiEvent, MidiPortInfo};

pub(crate) struct Port;

pub(crate) fn list() -> Result<Vec<MidiPortInfo>, MidiError> {
    Err(MidiError::UnsupportedPlatform)
}

pub(crate) fn open(_: &str, _: &str, _: Sender<MidiEvent>) -> Result<Port, MidiError> {
    Err(MidiError::UnsupportedPlatform)
}

pub(crate) struct OutPort;

pub(crate) fn list_outputs() -> Result<Vec<MidiPortInfo>, MidiError> {
    Err(MidiError::UnsupportedPlatform)
}

pub(crate) fn open_output(_: &str) -> Result<OutPort, MidiError> {
    Err(MidiError::UnsupportedPlatform)
}

impl OutPort {
    pub(crate) fn send(&mut self, _: &[u8]) -> Result<(), MidiError> {
        Err(MidiError::UnsupportedPlatform)
    }
}
