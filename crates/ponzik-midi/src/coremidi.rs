//! Pilote CoreMIDI : liaisons FFI maison vers `CoreMIDI`.
//!
//! Un client unique pour tout le processus ; chaque port ouvert crée un port
//! d'entrée relié à une source. CoreMIDI appelle notre rappel sur son propre
//! thread avec une liste de paquets, dont les octets passent par le même
//! analyseur que sous Linux. L'identifiant d'un port est l'identifiant unique
//! CoreMIDI de la source (stable d'une session à l'autre).
//!
//! En sortie, chaque port ouvert crée un port de sortie et envoie ses octets à
//! une destination par `MIDISend`, horodatés « maintenant » : c'est le thread
//! d'envoi de [`crate::out`] qui choisit l'instant.

#![allow(non_upper_case_globals)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::{null, null_mut};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use crate::parser::Parser;
use crate::{MidiError, MidiEvent, MidiPortInfo};

type OSStatus = i32;
type MIDIObjectRef = u32;
type MIDIClientRef = MIDIObjectRef;
type MIDIPortRef = MIDIObjectRef;
type MIDIEndpointRef = MIDIObjectRef;
type CFStringRef = *const c_void;

/// `MIDIPacketList` : lu octet par octet (structure compactée sur 4 octets).
#[repr(C)]
struct MIDIPacketList {
    _opaque: [u8; 0],
}

type MIDIReadProc = unsafe extern "C" fn(list: *const MIDIPacketList, port_ref: *mut c_void, source_ref: *mut c_void);
type MIDINotifyProc = unsafe extern "C" fn(message: *const c_void, ref_con: *mut c_void);

#[link(name = "CoreMIDI", kind = "framework")]
unsafe extern "C" {
    static kMIDIPropertyDisplayName: CFStringRef;
    static kMIDIPropertyName: CFStringRef;
    static kMIDIPropertyUniqueID: CFStringRef;

    fn MIDIClientCreate(
        name: CFStringRef,
        notify: Option<MIDINotifyProc>,
        ref_con: *mut c_void,
        client: *mut MIDIClientRef,
    ) -> OSStatus;
    fn MIDIInputPortCreate(
        client: MIDIClientRef,
        name: CFStringRef,
        read: MIDIReadProc,
        ref_con: *mut c_void,
        port: *mut MIDIPortRef,
    ) -> OSStatus;
    fn MIDIPortConnectSource(port: MIDIPortRef, source: MIDIEndpointRef, ref_con: *mut c_void) -> OSStatus;
    fn MIDIPortDispose(port: MIDIPortRef) -> OSStatus;
    fn MIDIGetNumberOfSources() -> usize;
    fn MIDIGetSource(index: usize) -> MIDIEndpointRef;
    fn MIDIOutputPortCreate(client: MIDIClientRef, name: CFStringRef, port: *mut MIDIPortRef) -> OSStatus;
    fn MIDIGetNumberOfDestinations() -> usize;
    fn MIDIGetDestination(index: usize) -> MIDIEndpointRef;
    fn MIDISend(port: MIDIPortRef, destination: MIDIEndpointRef, list: *const MIDIPacketList) -> OSStatus;
    fn MIDIObjectGetIntegerProperty(object: MIDIObjectRef, property: CFStringRef, value: *mut i32) -> OSStatus;
    fn MIDIObjectGetStringProperty(object: MIDIObjectRef, property: CFStringRef, value: *mut CFStringRef) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, string: *const c_char, encoding: u32) -> CFStringRef;
    fn CFStringGetCString(string: CFStringRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(object: *const c_void);
}

const kCFStringEncodingUTF8: u32 = 0x0800_0100;

fn error(code: OSStatus, call: &str) -> MidiError {
    MidiError::System(format!("{call} : OSStatus {code}"))
}

/// CFString possédée, libérée à la destruction.
struct CfString(CFStringRef);

impl CfString {
    fn new(s: &str) -> Self {
        let c = CString::new(s.replace('\0', " ")).unwrap_or_default();
        // SAFETY: chaîne C valide ; le résultat nous appartient (règle « Create »).
        Self(unsafe { CFStringCreateWithCString(null(), c.as_ptr(), kCFStringEncodingUTF8) })
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: référence possédée, libérée une seule fois.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn string_property(object: MIDIObjectRef, property: CFStringRef) -> Option<String> {
    let mut value: CFStringRef = null();
    // SAFETY: `value` reçoit une CFString possédée, rendue à `CfString`.
    let code = unsafe { MIDIObjectGetStringProperty(object, property, &mut value) };
    if code != 0 || value.is_null() {
        return None;
    }
    let value = CfString(value);
    let mut buf = [0 as c_char; 256];
    // SAFETY: tampon de taille connue, terminé par zéro en cas de succès.
    let ok = unsafe { CFStringGetCString(value.0, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8) };
    // SAFETY: idem.
    (ok != 0).then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().trim().to_owned())
}

fn unique_id(object: MIDIObjectRef) -> Option<i32> {
    let mut id = 0;
    // SAFETY: propriété entière, statique de CoreMIDI.
    let code = unsafe { MIDIObjectGetIntegerProperty(object, kMIDIPropertyUniqueID, &mut id) };
    (code == 0).then_some(id)
}

/// Sources MIDI du système, avec leur identifiant unique.
fn sources() -> Vec<(MIDIEndpointRef, MidiPortInfo)> {
    // SAFETY: énumération sans état.
    let count = unsafe { MIDIGetNumberOfSources() };
    // SAFETY: index dans les bornes (vérifiées par CoreMIDI, 0 sinon).
    endpoints(count, |i| unsafe { MIDIGetSource(i) }, "Source MIDI")
}

/// Destinations MIDI du système, avec leur identifiant unique.
fn destinations() -> Vec<(MIDIEndpointRef, MidiPortInfo)> {
    // SAFETY: énumération sans état.
    let count = unsafe { MIDIGetNumberOfDestinations() };
    // SAFETY: idem.
    endpoints(count, |i| unsafe { MIDIGetDestination(i) }, "MIDI destination")
}

fn endpoints(
    count: usize,
    get: impl Fn(usize) -> MIDIEndpointRef,
    fallback: &str,
) -> Vec<(MIDIEndpointRef, MidiPortInfo)> {
    (0..count)
        .filter_map(|i| {
            let endpoint = get(i);
            if endpoint == 0 {
                return None;
            }
            let id = unique_id(endpoint)?;
            // SAFETY: constantes exportées par CoreMIDI.
            let (display, plain) = unsafe { (kMIDIPropertyDisplayName, kMIDIPropertyName) };
            let name = string_property(endpoint, display)
                .or_else(|| string_property(endpoint, plain))
                .unwrap_or_else(|| format!("{fallback} {id}"));
            Some((endpoint, MidiPortInfo { id: id.to_string(), name }))
        })
        .collect()
}

/// Client CoreMIDI du processus, créé à la première utilisation.
fn client() -> Result<MIDIClientRef, MidiError> {
    static CLIENT: OnceLock<Result<MIDIClientRef, OSStatus>> = OnceLock::new();
    let result = CLIENT.get_or_init(|| {
        let name = CfString::new("Ponzik");
        let mut client = 0;
        // SAFETY: pas de rappel de notification ; `client` reçoit la référence.
        match unsafe { MIDIClientCreate(name.0, None, null_mut(), &mut client) } {
            0 => Ok(client),
            code => Err(code),
        }
    });
    (*result).map_err(|code| error(code, "MIDIClientCreate"))
}

pub(crate) fn list() -> Result<Vec<MidiPortInfo>, MidiError> {
    client()?;
    Ok(sources().into_iter().map(|(_, info)| info).collect())
}

struct Reader {
    parser: Parser,
    tx: Sender<MidiEvent>,
}

/// Rappel CoreMIDI : thread système, ne doit ni paniquer ni bloquer longtemps.
unsafe extern "C" fn read_proc(list: *const MIDIPacketList, port_ref: *mut c_void, _: *mut c_void) {
    // SAFETY: `port_ref` est le `Mutex<Reader>` de `Port`, libéré après MIDIPortDispose.
    let reader = unsafe { &*port_ref.cast::<Mutex<Reader>>() };
    let Ok(mut reader) = reader.lock() else { return };
    let Reader { parser, tx } = &mut *reader;
    let base = list.cast::<u8>();
    // SAFETY: disposition de MIDIServices.h (`#pragma pack(4)`) : `numPackets` (u32),
    // puis des paquets `timeStamp` (u64), `length` (u16), `data[length]`.
    unsafe {
        let count = base.cast::<u32>().read_unaligned();
        let mut packet = base.add(4);
        for _ in 0..count {
            let len = packet.add(8).cast::<u16>().read_unaligned() as usize;
            let data = std::slice::from_raw_parts(packet.add(10), len);
            for &byte in data {
                if let Some(event) = parser.push(byte) {
                    let _ = tx.send(event);
                }
            }
            // MIDIPacketNext : aligné sur 4 octets sur ARM, collé ailleurs.
            let next = packet.add(10 + len);
            packet = if cfg!(any(target_arch = "aarch64", target_arch = "arm")) {
                next.add((4 - (next as usize & 3)) & 3)
            } else {
                next
            };
        }
    }
}

pub(crate) struct Port {
    port: MIDIPortRef,
    reader: *mut Mutex<Reader>,
}

// SAFETY: les références CoreMIDI s'utilisent depuis n'importe quel thread ;
// `reader` n'est lu que par le rappel, derrière son verrou.
unsafe impl Send for Port {}

impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: port créé par MIDIInputPortCreate ; après sa destruction plus
        // aucun rappel n'utilise `reader`.
        unsafe {
            MIDIPortDispose(self.port);
            drop(Box::from_raw(self.reader));
        }
    }
}

pub(crate) fn open(id: &str, name: &str, tx: Sender<MidiEvent>) -> Result<Port, MidiError> {
    let client = client()?;
    let source = sources()
        .into_iter()
        .find(|(_, info)| info.id == id)
        .map(|(source, _)| source)
        .ok_or_else(|| MidiError::PortNotFound(id.to_owned()))?;
    let reader = Box::into_raw(Box::new(Mutex::new(Reader { parser: Parser::new(), tx })));
    let port_name = CfString::new(&format!("Ponzik · {name}"));
    let mut port = 0;
    // SAFETY: `reader` reste valide jusqu'à la destruction de `Port`.
    let code = unsafe { MIDIInputPortCreate(client, port_name.0, read_proc, reader.cast(), &mut port) };
    if code != 0 {
        // SAFETY: aucun port créé, aucun rappel ne détient `reader`.
        drop(unsafe { Box::from_raw(reader) });
        return Err(error(code, "MIDIInputPortCreate"));
    }
    let port = Port { port, reader };
    // SAFETY: port et source valides.
    match unsafe { MIDIPortConnectSource(port.port, source, null_mut()) } {
        0 => Ok(port),
        code => Err(error(code, "MIDIPortConnectSource")),
    }
}

pub(crate) fn list_outputs() -> Result<Vec<MidiPortInfo>, MidiError> {
    client()?;
    Ok(destinations().into_iter().map(|(_, info)| info).collect())
}

/// Octets de données au plus par paquet envoyé.
const PACKET_DATA: usize = 256;

/// `MIDIPacketList` d'un seul paquet, construit à la main (`#pragma pack(4)`) :
/// `numPackets` (u32), `timeStamp` (u64), `length` (u16), `data`.
#[repr(C, align(8))]
struct PacketBuffer([u8; 4 + 8 + 2 + PACKET_DATA]);

pub(crate) struct OutPort {
    port: MIDIPortRef,
    destination: MIDIEndpointRef,
    buffer: Box<PacketBuffer>,
}

// SAFETY: les références CoreMIDI s'utilisent depuis n'importe quel thread.
unsafe impl Send for OutPort {}

impl Drop for OutPort {
    fn drop(&mut self) {
        // SAFETY: port créé par MIDIOutputPortCreate, détruit une seule fois.
        unsafe { MIDIPortDispose(self.port) };
    }
}

pub(crate) fn open_output(id: &str) -> Result<OutPort, MidiError> {
    let client = client()?;
    let (destination, info) = destinations()
        .into_iter()
        .find(|(_, info)| info.id == id)
        .ok_or_else(|| MidiError::PortNotFound(id.to_owned()))?;
    let port_name = CfString::new(&format!("Ponzik · {}", info.name));
    let mut port = 0;
    // SAFETY: client valide ; `port` reçoit la référence.
    match unsafe { MIDIOutputPortCreate(client, port_name.0, &mut port) } {
        0 => Ok(OutPort { port, destination, buffer: Box::new(PacketBuffer([0; 4 + 8 + 2 + PACKET_DATA])) }),
        code => Err(error(code, "MIDIOutputPortCreate")),
    }
}

impl OutPort {
    pub(crate) fn send(&mut self, bytes: &[u8]) -> Result<(), MidiError> {
        for chunk in bytes.chunks(PACKET_DATA) {
            let buf = &mut self.buffer.0;
            buf[..4].copy_from_slice(&1u32.to_ne_bytes());
            // Horodatage nul : envoi immédiat.
            buf[4..12].copy_from_slice(&0u64.to_ne_bytes());
            buf[12..14].copy_from_slice(&(chunk.len() as u16).to_ne_bytes());
            buf[14..14 + chunk.len()].copy_from_slice(chunk);
            // SAFETY: liste d'un paquet bien formée, valide pendant l'appel.
            let code = unsafe { MIDISend(self.port, self.destination, buf.as_ptr().cast()) };
            if code != 0 {
                return Err(error(code, "MIDISend"));
            }
        }
        Ok(())
    }
}
