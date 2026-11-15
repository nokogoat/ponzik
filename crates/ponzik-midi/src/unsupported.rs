use std::sync::mpsc::Sender;

use crate::{MidiError, MidiEvent, MidiPortInfo};

pub(crate) struct Port;

pub(crate) fn list() -> Result<Vec<MidiPortInfo>, MidiError> {
    Err(MidiError::UnsupportedPlatform)
}

pub(crate) fn open(_: &str, _: &str, _: Sender<MidiEvent>) -> Result<Port, MidiError> {
    Err(MidiError::UnsupportedPlatform)
}
