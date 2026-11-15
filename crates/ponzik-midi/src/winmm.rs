//! Pilote WinMM : `midiIn*` déclarées à la main, sans crate `windows`.
//!
//! Le système appelle notre rappel sur son propre thread pour chaque message
//! court ; il le décode et l'envoie dans le canal. L'identifiant d'un port est
//! son numéro WinMM (« 0 », « 1 »…). Les SysEx sont ignorés (aucun tampon fourni).

#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::mpsc::Sender;

use crate::parser::decode;
use crate::{MidiError, MidiEvent, MidiPortInfo};

type HMIDIIN = *mut c_void;
type MMRESULT = u32;

#[repr(C)]
struct MidiInCapsW {
    mid: u16,
    pid: u16,
    driver_version: u32,
    pname: [u16; 32],
    support: u32,
}

type MidiInProc = unsafe extern "system" fn(HMIDIIN, u32, usize, usize, usize);

#[link(name = "winmm")]
unsafe extern "system" {
    fn midiInGetNumDevs() -> u32;
    fn midiInGetDevCapsW(device: usize, caps: *mut MidiInCapsW, size: u32) -> MMRESULT;
    fn midiInOpen(handle: *mut HMIDIIN, device: u32, callback: usize, instance: usize, flags: u32) -> MMRESULT;
    fn midiInStart(handle: HMIDIIN) -> MMRESULT;
    fn midiInStop(handle: HMIDIIN) -> MMRESULT;
    fn midiInReset(handle: HMIDIIN) -> MMRESULT;
    fn midiInClose(handle: HMIDIIN) -> MMRESULT;
}

const MMSYSERR_NOERROR: MMRESULT = 0;
const MMSYSERR_BADDEVICEID: MMRESULT = 2;
const MMSYSERR_ALLOCATED: MMRESULT = 4;
const CALLBACK_FUNCTION: u32 = 0x0003_0000;
const MIM_DATA: u32 = 0x3C3;

fn error(code: MMRESULT, call: &str) -> MidiError {
    match code {
        MMSYSERR_ALLOCATED => MidiError::System(format!("{call} : port déjà utilisé par une autre application")),
        _ => MidiError::System(format!("{call} : code MMRESULT {code}")),
    }
}

fn device_name(index: u32) -> Option<String> {
    // SAFETY: structure POD, zéro est une valeur valide.
    let mut caps: MidiInCapsW = unsafe { std::mem::zeroed() };
    // SAFETY: `caps` est valide et sa taille est passée.
    let code = unsafe { midiInGetDevCapsW(index as usize, &mut caps, size_of::<MidiInCapsW>() as u32) };
    if code != MMSYSERR_NOERROR {
        return None;
    }
    let len = caps.pname.iter().position(|&c| c == 0).unwrap_or(caps.pname.len());
    Some(String::from_utf16_lossy(&caps.pname[..len]).trim().to_owned())
}

pub(crate) fn list() -> Result<Vec<MidiPortInfo>, MidiError> {
    // SAFETY: sans argument.
    let count = unsafe { midiInGetNumDevs() };
    Ok((0..count)
        .filter_map(|i| device_name(i).map(|name| MidiPortInfo { id: i.to_string(), name }))
        .collect())
}

/// Rappel WinMM : thread système, ne doit ni paniquer ni bloquer.
unsafe extern "system" fn midi_in_proc(_: HMIDIIN, msg: u32, instance: usize, param1: usize, _: usize) {
    if msg != MIM_DATA {
        return;
    }
    // SAFETY: `instance` est le `Box<Sender>` de `Port`, libéré seulement après midiInClose.
    let tx = unsafe { &*(instance as *const Sender<MidiEvent>) };
    let [status, d1, d2, _] = (param1 as u32).to_le_bytes();
    if let Some(event) = decode(status, d1, d2) {
        let _ = tx.send(event);
    }
}

pub(crate) struct Port {
    handle: HMIDIIN,
    tx: *mut Sender<MidiEvent>,
}

// SAFETY: le handle WinMM s'utilise depuis n'importe quel thread ; `tx` n'est lu que par le rappel.
unsafe impl Send for Port {}

impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: handle ouvert ; après midiInClose plus aucun rappel n'utilise `tx`.
        unsafe {
            midiInStop(self.handle);
            midiInReset(self.handle);
            midiInClose(self.handle);
            drop(Box::from_raw(self.tx));
        }
    }
}

pub(crate) fn open(id: &str, _name: &str, tx: Sender<MidiEvent>) -> Result<Port, MidiError> {
    let not_found = || MidiError::PortNotFound(id.to_owned());
    let index: u32 = id.parse().map_err(|_| not_found())?;
    let tx = Box::into_raw(Box::new(tx));
    let mut handle = null_mut();
    let callback: MidiInProc = midi_in_proc;
    // SAFETY: `tx` reste valide jusqu'à la destruction de `Port`.
    let code = unsafe { midiInOpen(&mut handle, index, callback as usize, tx as usize, CALLBACK_FUNCTION) };
    if code != MMSYSERR_NOERROR {
        // SAFETY: l'ouverture a échoué, aucun rappel ne détient `tx`.
        drop(unsafe { Box::from_raw(tx) });
        return Err(if code == MMSYSERR_BADDEVICEID { not_found() } else { error(code, "midiInOpen") });
    }
    let port = Port { handle, tx };
    // SAFETY: handle ouvert.
    match unsafe { midiInStart(port.handle) } {
        MMSYSERR_NOERROR => Ok(port),
        code => Err(error(code, "midiInStart")),
    }
}
