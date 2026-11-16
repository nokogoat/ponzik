//! Pilote WinMM : `midiIn*` déclarées à la main, sans crate `windows`.
//!
//! Le système appelle notre rappel sur son propre thread pour chaque message
//! court ; il le décode et l'envoie dans le canal. L'identifiant d'un port est
//! son numéro WinMM (« 0 », « 1 »…). Les SysEx sont ignorés (aucun tampon fourni).
//!
//! En sortie (`midiOut*`), les messages courts partent par `midiOutShortMsg`,
//! les plus longs (SysEx) par `midiOutLongMsg` avec un en-tête préparé, attendu
//! jusqu'à la fin de l'envoi : on écrit depuis le thread d'envoi, jamais depuis
//! le thread audio.

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

type HMIDIOUT = *mut c_void;

#[repr(C)]
struct MidiOutCapsW {
    mid: u16,
    pid: u16,
    driver_version: u32,
    pname: [u16; 32],
    technology: u16,
    voices: u16,
    notes: u16,
    channel_mask: u16,
    support: u32,
}

/// `MIDIHDR` de mmsystem.h.
#[repr(C)]
struct MidiHdr {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    next: *mut MidiHdr,
    reserved: usize,
    offset: u32,
    reserved2: [usize; 8],
}

#[link(name = "winmm")]
unsafe extern "system" {
    fn midiInGetNumDevs() -> u32;
    fn midiInGetDevCapsW(device: usize, caps: *mut MidiInCapsW, size: u32) -> MMRESULT;
    fn midiInOpen(handle: *mut HMIDIIN, device: u32, callback: usize, instance: usize, flags: u32) -> MMRESULT;
    fn midiInStart(handle: HMIDIIN) -> MMRESULT;
    fn midiInStop(handle: HMIDIIN) -> MMRESULT;
    fn midiInReset(handle: HMIDIIN) -> MMRESULT;
    fn midiInClose(handle: HMIDIIN) -> MMRESULT;
    fn midiOutGetNumDevs() -> u32;
    fn midiOutGetDevCapsW(device: usize, caps: *mut MidiOutCapsW, size: u32) -> MMRESULT;
    fn midiOutOpen(handle: *mut HMIDIOUT, device: u32, callback: usize, instance: usize, flags: u32) -> MMRESULT;
    fn midiOutShortMsg(handle: HMIDIOUT, msg: u32) -> MMRESULT;
    fn midiOutLongMsg(handle: HMIDIOUT, hdr: *mut MidiHdr, size: u32) -> MMRESULT;
    fn midiOutPrepareHeader(handle: HMIDIOUT, hdr: *mut MidiHdr, size: u32) -> MMRESULT;
    fn midiOutUnprepareHeader(handle: HMIDIOUT, hdr: *mut MidiHdr, size: u32) -> MMRESULT;
    fn midiOutReset(handle: HMIDIOUT) -> MMRESULT;
    fn midiOutClose(handle: HMIDIOUT) -> MMRESULT;
}

const MMSYSERR_NOERROR: MMRESULT = 0;
const MMSYSERR_BADDEVICEID: MMRESULT = 2;
const MMSYSERR_ALLOCATED: MMRESULT = 4;
const CALLBACK_FUNCTION: u32 = 0x0003_0000;
const MIM_DATA: u32 = 0x3C3;
const CALLBACK_NULL: u32 = 0;
const MIDIERR_STILLPLAYING: MMRESULT = 65;

fn error(code: MMRESULT, call: &str) -> MidiError {
    match code {
        MMSYSERR_ALLOCATED => MidiError::System(format!("{call}: port already used by another application")),
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

fn output_name(index: u32) -> Option<String> {
    // SAFETY: structure POD, zéro est une valeur valide.
    let mut caps: MidiOutCapsW = unsafe { std::mem::zeroed() };
    // SAFETY: `caps` est valide et sa taille est passée.
    let code = unsafe { midiOutGetDevCapsW(index as usize, &mut caps, size_of::<MidiOutCapsW>() as u32) };
    if code != MMSYSERR_NOERROR {
        return None;
    }
    let len = caps.pname.iter().position(|&c| c == 0).unwrap_or(caps.pname.len());
    Some(String::from_utf16_lossy(&caps.pname[..len]).trim().to_owned())
}

pub(crate) fn list_outputs() -> Result<Vec<MidiPortInfo>, MidiError> {
    // SAFETY: sans argument.
    let count = unsafe { midiOutGetNumDevs() };
    Ok((0..count)
        .filter_map(|i| output_name(i).map(|name| MidiPortInfo { id: i.to_string(), name }))
        .collect())
}

pub(crate) struct OutPort {
    handle: HMIDIOUT,
}

// SAFETY: le handle WinMM s'utilise depuis n'importe quel thread.
unsafe impl Send for OutPort {}

impl Drop for OutPort {
    fn drop(&mut self) {
        // SAFETY: handle ouvert par midiOutOpen, fermé une seule fois.
        unsafe {
            midiOutReset(self.handle);
            midiOutClose(self.handle);
        }
    }
}

pub(crate) fn open_output(id: &str) -> Result<OutPort, MidiError> {
    let not_found = || MidiError::PortNotFound(id.to_owned());
    let index: u32 = id.parse().map_err(|_| not_found())?;
    let mut handle = null_mut();
    // SAFETY: pas de rappel ; `handle` reçoit le port ouvert.
    let code = unsafe { midiOutOpen(&mut handle, index, 0, 0, CALLBACK_NULL) };
    match code {
        MMSYSERR_NOERROR => Ok(OutPort { handle }),
        MMSYSERR_BADDEVICEID => Err(not_found()),
        code => Err(error(code, "midiOutOpen")),
    }
}

impl OutPort {
    pub(crate) fn send(&mut self, bytes: &[u8]) -> Result<(), MidiError> {
        match bytes {
            [] => Ok(()),
            // Message court : statut et données tassés dans un mot, poids faible d'abord.
            [status, data @ ..] if data.len() <= 2 && *status != 0xF0 => {
                let msg = u32::from_le_bytes([*status, data.first().copied().unwrap_or(0), data.get(1).copied().unwrap_or(0), 0]);
                // SAFETY: handle ouvert.
                match unsafe { midiOutShortMsg(self.handle, msg) } {
                    MMSYSERR_NOERROR => Ok(()),
                    code => Err(error(code, "midiOutShortMsg")),
                }
            }
            _ => self.send_long(bytes),
        }
    }

    /// SysEx ou suite de messages : tampon préparé, envoi, attente de la fin.
    fn send_long(&mut self, bytes: &[u8]) -> Result<(), MidiError> {
        let mut data = bytes.to_vec();
        // SAFETY: structure POD, zéro est une valeur valide.
        let mut hdr: MidiHdr = unsafe { std::mem::zeroed() };
        hdr.data = data.as_mut_ptr();
        hdr.buffer_length = data.len() as u32;
        hdr.bytes_recorded = data.len() as u32;
        let size = size_of::<MidiHdr>() as u32;
        // SAFETY: `hdr` et `data` restent en place jusqu'à midiOutUnprepareHeader.
        unsafe {
            let code = midiOutPrepareHeader(self.handle, &mut hdr, size);
            if code != MMSYSERR_NOERROR {
                return Err(error(code, "midiOutPrepareHeader"));
            }
            let sent = midiOutLongMsg(self.handle, &mut hdr, size);
            while midiOutUnprepareHeader(self.handle, &mut hdr, size) == MIDIERR_STILLPLAYING {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            if sent != MMSYSERR_NOERROR {
                return Err(error(sent, "midiOutLongMsg"));
            }
        }
        Ok(())
    }
}
