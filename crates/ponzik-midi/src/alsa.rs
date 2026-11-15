//! Pilote ALSA rawmidi : liaisons FFI maison vers `libasound`.
//!
//! On énumère les sous-périphériques d'entrée de chaque carte via l'interface
//! de contrôle, puis on ouvre `hw:C,D,S` en non bloquant. Le thread de lecture
//! attend avec `poll` (délai court) pour pouvoir s'arrêter proprement.

use std::ffi::{CStr, CString, c_char, c_int, c_short, c_uint, c_ulong, c_void};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use crate::parser::Parser;
use crate::{MidiError, MidiEvent, MidiPortInfo};

#[repr(C)]
struct SndCtl {
    _opaque: [u8; 0],
}

#[repr(C)]
struct SndRawmidi {
    _opaque: [u8; 0],
}

#[repr(C)]
struct SndRawmidiInfo {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

#[link(name = "asound")]
unsafe extern "C" {
    fn snd_card_next(card: *mut c_int) -> c_int;
    fn snd_ctl_open(ctl: *mut *mut SndCtl, name: *const c_char, mode: c_int) -> c_int;
    fn snd_ctl_close(ctl: *mut SndCtl) -> c_int;
    fn snd_ctl_rawmidi_next_device(ctl: *mut SndCtl, device: *mut c_int) -> c_int;
    fn snd_ctl_rawmidi_info(ctl: *mut SndCtl, info: *mut SndRawmidiInfo) -> c_int;
    fn snd_rawmidi_info_malloc(info: *mut *mut SndRawmidiInfo) -> c_int;
    fn snd_rawmidi_info_free(info: *mut SndRawmidiInfo);
    fn snd_rawmidi_info_set_device(info: *mut SndRawmidiInfo, device: c_uint);
    fn snd_rawmidi_info_set_subdevice(info: *mut SndRawmidiInfo, subdevice: c_uint);
    fn snd_rawmidi_info_set_stream(info: *mut SndRawmidiInfo, stream: c_int);
    fn snd_rawmidi_info_get_subdevices_count(info: *const SndRawmidiInfo) -> c_uint;
    fn snd_rawmidi_info_get_name(info: *const SndRawmidiInfo) -> *const c_char;
    fn snd_rawmidi_info_get_subdevice_name(info: *const SndRawmidiInfo) -> *const c_char;
    fn snd_rawmidi_open(
        input: *mut *mut SndRawmidi,
        output: *mut *mut SndRawmidi,
        name: *const c_char,
        mode: c_int,
    ) -> c_int;
    fn snd_rawmidi_close(rawmidi: *mut SndRawmidi) -> c_int;
    fn snd_rawmidi_read(rawmidi: *mut SndRawmidi, buffer: *mut c_void, size: usize) -> isize;
    fn snd_rawmidi_poll_descriptors_count(rawmidi: *mut SndRawmidi) -> c_int;
    fn snd_rawmidi_poll_descriptors(rawmidi: *mut SndRawmidi, pfds: *mut PollFd, space: c_uint) -> c_int;
    fn snd_strerror(errnum: c_int) -> *const c_char;
}

// libc, toujours liée par std.
unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

const SND_RAWMIDI_STREAM_INPUT: c_int = 1;
const SND_RAWMIDI_NONBLOCK: c_int = 0x0002;
const EAGAIN: c_int = 11;
const ENOENT: c_int = 2;
const POLL_TIMEOUT_MS: c_int = 50;

fn check(code: c_int, call: &str) -> Result<(), MidiError> {
    if code >= 0 {
        return Ok(());
    }
    Err(MidiError::System(format!("{call} : {}", strerror(code))))
}

fn strerror(code: c_int) -> String {
    // SAFETY: snd_strerror renvoie une chaîne statique.
    unsafe { CStr::from_ptr(snd_strerror(code)) }.to_string_lossy().into_owned()
}

/// Chaîne C possédée par ALSA (peut être nulle) → `String`.
fn c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: pointeur non nul vers une chaîne terminée, valide pendant l'appel.
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().trim().to_owned()
}

struct Ctl(*mut SndCtl);

impl Drop for Ctl {
    fn drop(&mut self) {
        // SAFETY: handle ouvert par snd_ctl_open, fermé une seule fois.
        unsafe { snd_ctl_close(self.0) };
    }
}

struct Info(*mut SndRawmidiInfo);

impl Drop for Info {
    fn drop(&mut self) {
        // SAFETY: alloué par snd_rawmidi_info_malloc.
        unsafe { snd_rawmidi_info_free(self.0) };
    }
}

pub(crate) fn list() -> Result<Vec<MidiPortInfo>, MidiError> {
    let mut raw = null_mut();
    // SAFETY: pointeur de sortie valide.
    check(unsafe { snd_rawmidi_info_malloc(&mut raw) }, "snd_rawmidi_info_malloc")?;
    let info = Info(raw);
    let mut ports = Vec::new();
    let mut card: c_int = -1;
    loop {
        // SAFETY: `card` valide ; -1 en sortie = plus de carte.
        if unsafe { snd_card_next(&mut card) } < 0 || card < 0 {
            break;
        }
        // Une carte illisible ne doit pas masquer les autres.
        list_card(card, &info, &mut ports);
    }
    Ok(ports)
}

fn list_card(card: c_int, info: &Info, ports: &mut Vec<MidiPortInfo>) {
    let Ok(hw) = CString::new(format!("hw:{card}")) else { return };
    let mut raw = null_mut();
    // SAFETY: nom C valide, pointeur de sortie valide.
    if unsafe { snd_ctl_open(&mut raw, hw.as_ptr(), 0) } < 0 {
        return;
    }
    let ctl = Ctl(raw);
    let mut device: c_int = -1;
    loop {
        // SAFETY: handle ouvert ; -1 en sortie = plus de périphérique.
        if unsafe { snd_ctl_rawmidi_next_device(ctl.0, &mut device) } < 0 || device < 0 {
            break;
        }
        // SAFETY: `info` alloué, `ctl` ouvert.
        let count = unsafe {
            snd_rawmidi_info_set_device(info.0, device as c_uint);
            snd_rawmidi_info_set_subdevice(info.0, 0);
            snd_rawmidi_info_set_stream(info.0, SND_RAWMIDI_STREAM_INPUT);
            if snd_ctl_rawmidi_info(ctl.0, info.0) < 0 {
                continue; // -ENOENT : sortie seule
            }
            snd_rawmidi_info_get_subdevices_count(info.0)
        };
        for sub in 0..count {
            // SAFETY: idem.
            let name = unsafe {
                snd_rawmidi_info_set_subdevice(info.0, sub);
                let code = snd_ctl_rawmidi_info(ctl.0, info.0);
                if code < 0 && code != -ENOENT {
                    continue;
                }
                let dev = c_str(snd_rawmidi_info_get_name(info.0));
                let sub_name = c_str(snd_rawmidi_info_get_subdevice_name(info.0));
                if count > 1 && !sub_name.is_empty() { sub_name } else { dev }
            };
            let id = format!("hw:{card},{device},{sub}");
            let name = if name.is_empty() { id.clone() } else { format!("{id} — {name}") };
            ports.push(MidiPortInfo { id, name });
        }
    }
}

struct Rawmidi(*mut SndRawmidi);

// SAFETY: le handle n'est utilisé que par un thread à la fois (ouverture, puis lecture).
unsafe impl Send for Rawmidi {}

impl Drop for Rawmidi {
    fn drop(&mut self) {
        // SAFETY: handle ouvert par snd_rawmidi_open, fermé une seule fois.
        unsafe { snd_rawmidi_close(self.0) };
    }
}

pub(crate) struct Port {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Port {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub(crate) fn open(id: &str, name: &str, tx: Sender<MidiEvent>) -> Result<Port, MidiError> {
    let c_id = CString::new(id).map_err(|_| MidiError::PortNotFound(id.to_owned()))?;
    let mut raw = null_mut();
    // SAFETY: nom C valide ; seule l'entrée est demandée.
    let code = unsafe { snd_rawmidi_open(&mut raw, null_mut(), c_id.as_ptr(), SND_RAWMIDI_NONBLOCK) };
    if code == -ENOENT {
        return Err(MidiError::PortNotFound(id.to_owned()));
    }
    check(code, &format!("snd_rawmidi_open {id}"))?;
    let midi = Rawmidi(raw);

    // SAFETY: handle ouvert ; `fds` a la place annoncée.
    let fds = unsafe {
        let count = snd_rawmidi_poll_descriptors_count(midi.0).max(0) as usize;
        let mut fds = vec![PollFd { fd: -1, events: 0, revents: 0 }; count];
        let filled = snd_rawmidi_poll_descriptors(midi.0, fds.as_mut_ptr(), count as c_uint);
        check(filled, "snd_rawmidi_poll_descriptors")?;
        fds.truncate(filled as usize);
        fds
    };

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let label = name.to_owned();
    let handle = thread::Builder::new()
        .name("ponzik-midi".into())
        .spawn(move || read_loop(midi, fds, &flag, &tx, &label))
        .map_err(|e| MidiError::System(format!("thread MIDI : {e}")))?;
    Ok(Port { stop, handle: Some(handle) })
}

fn read_loop(midi: Rawmidi, mut fds: Vec<PollFd>, stop: &AtomicBool, tx: &Sender<MidiEvent>, name: &str) {
    let mut parser = Parser::new();
    let mut buf = [0u8; 256];
    let mut events = Vec::new();
    while !stop.load(Ordering::Acquire) {
        // SAFETY: `fds` est un tableau valide de `fds.len()` entrées.
        unsafe { poll(fds.as_mut_ptr(), fds.len() as c_ulong, POLL_TIMEOUT_MS) };
        loop {
            // SAFETY: `buf` est un tampon valide de `buf.len()` octets.
            let n = unsafe { snd_rawmidi_read(midi.0, buf.as_mut_ptr().cast(), buf.len()) };
            if n == -(EAGAIN as isize) {
                break;
            }
            if n < 0 {
                // Périphérique débranché ou erreur : on arrête ce port.
                eprintln!("ponzik-midi : {name} : {}", strerror(n as c_int));
                return;
            }
            if n == 0 {
                break;
            }
            parser.feed(&buf[..n as usize], &mut events);
            for e in events.drain(..) {
                if tx.send(e).is_err() {
                    return;
                }
            }
        }
    }
}
